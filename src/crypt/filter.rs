//! Git protocol plumbing never prints presentation output or prompts.
//! Owned packet/input/output buffers are wiped, including retired growth allocations.
//! Age envelope internals are library-owned and outside this buffer guarantee.

use std::io::{Read, Write};

use anyhow::{Context as _, Result, bail, ensure};
use zeroize::Zeroizing;

use super::buffer::reserve_secret;
use super::{access, format};

pub fn transform(repo: &git2::Repository, clean: bool, input: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let _guard = access::guard(repo, false)?;
    if format::recognized(input) {
        format::inspect(input)?;
        // A locked checkout may visit blobs before checking out their access metadata.
        if !clean && access::local_key_missing(repo)? {
            return Ok(Zeroizing::new(input.to_vec()));
        }
        let registry = access::load(repo)?;
        ensure!(
            input[10..format::HEADER_LEN] == registry.generation,
            "ciphertext has a stale key generation"
        );
        match access::local_key(repo) {
            Ok(key) => {
                let domain = format::file_domain(input)?;
                let plaintext = key.decrypt(input, domain)?;
                // The native store remains ciphertext on disk even while unlocked.
                if clean || domain == 1 {
                    Ok(Zeroizing::new(input.to_vec()))
                } else {
                    Ok(plaintext)
                }
            }
            Err(error) => {
                if access::local_key_missing(repo)? {
                    Ok(Zeroizing::new(input.to_vec()))
                } else {
                    Err(error)
                }
            }
        }
    } else if clean {
        access::local_key(repo)?
            .encrypt(input, 0)
            .map(Zeroizing::new)
    } else {
        // Historical plaintext is preserved for recovery; doctor identifies exposure.
        eprintln!(
            "over: protected Git blob is plaintext; run over doctor to inspect stored exposure"
        );
        Ok(Zeroizing::new(input.to_vec()))
    }
}

pub fn once(clean: bool) -> Result<()> {
    let repo = git2::Repository::open_from_env().or_else(|_| git2::Repository::discover("."))?;
    let input = read_input(
        &mut std::io::stdin().lock(),
        format::MAX_BYTES + format::HEADER_LEN + 16,
    )?;
    let output = transform(&repo, clean, &input)?;
    std::io::stdout().write_all(&output)?;
    Ok(())
}

fn append_secret(bytes: &mut Zeroizing<Vec<u8>>, extra: &[u8], limit: usize) -> Result<()> {
    let required = bytes
        .len()
        .checked_add(extra.len())
        .context("filter input size overflow")?;
    ensure!(required <= limit, "Git filter section exceeds size limit");
    drop(reserve_secret(bytes, required));
    bytes.extend_from_slice(extra);
    Ok(())
}

fn read_input(input: &mut impl Read, limit: usize) -> Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    let mut chunk = Zeroizing::new([0; 8192]);
    loop {
        // Read just one byte beyond the bound to reject oversize input without consuming it all.
        let available = chunk.len().min(limit - bytes.len() + 1);
        let length = match input.read(&mut chunk[..available]) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if length == 0 {
            return Ok(bytes);
        }
        append_secret(&mut bytes, &chunk[..length], limit)?;
    }
}

enum Packet {
    Eof,
    Flush,
    Data(Zeroizing<Vec<u8>>),
}

fn packet(input: &mut impl Read) -> Result<Packet> {
    let mut length = [0; 4];
    // Only EOF before the first header byte is a clean end of the request stream.
    loop {
        match input.read(&mut length[..1]) {
            Ok(0) => return Ok(Packet::Eof),
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    input.read_exact(&mut length[1..])?;
    ensure!(
        length.iter().all(u8::is_ascii_hexdigit),
        "invalid Git packet length"
    );
    let length = usize::from_str_radix(std::str::from_utf8(&length)?, 16)?;
    if length == 0 {
        return Ok(Packet::Flush);
    }
    ensure!((4..=65520).contains(&length), "invalid Git packet length");
    let mut bytes = Zeroizing::new(vec![0; length - 4]);
    input.read_exact(&mut bytes)?;
    Ok(Packet::Data(bytes))
}

fn section(input: &mut impl Read, limit: usize) -> Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    loop {
        match packet(input)? {
            Packet::Data(packet) => append_secret(&mut bytes, &packet, limit)?,
            Packet::Flush => return Ok(bytes),
            Packet::Eof => bail!("truncated Git filter section"),
        }
    }
}

fn write_packet(output: &mut impl Write, bytes: &[u8]) -> Result<()> {
    for chunk in bytes.chunks(65516) {
        write!(output, "{:04x}", chunk.len() + 4)?;
        output.write_all(chunk)?;
    }
    Ok(())
}

pub fn process() -> Result<()> {
    let repo = git2::Repository::open_from_env().or_else(|_| git2::Repository::discover("."))?;
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    process_io(&repo, &mut input, &mut output)
}

fn process_io(
    repo: &git2::Repository,
    mut input: &mut impl Read,
    mut output: &mut impl Write,
) -> Result<()> {
    let hello = section(&mut input, 4096)?;
    ensure!(
        hello.starts_with(b"git-filter-client\n")
            && hello
                .split(|b| *b == b'\n')
                .any(|line| line == b"version=2"),
        "unsupported Git filter handshake"
    );
    write_packet(&mut output, b"git-filter-server\n")?;
    write_packet(&mut output, b"version=2\n")?;
    output.write_all(b"0000")?;
    output.flush()?;
    let capabilities = section(&mut input, 4096)?;
    for capability in [
        b"capability=clean\n".as_slice(),
        b"capability=smudge\n".as_slice(),
    ] {
        if capabilities
            .windows(capability.len())
            .any(|bytes| bytes == capability)
        {
            write_packet(&mut output, capability)?;
        }
    }
    output.write_all(b"0000")?;
    output.flush()?;
    loop {
        let first = match packet(&mut input)? {
            Packet::Data(first) => first,
            Packet::Eof => return Ok(()),
            Packet::Flush => bail!("missing filter request"),
        };
        let rest = section(&mut input, 64 * 1024)?;
        let mut headers = first;
        append_secret(&mut headers, &rest, 64 * 1024)?;
        let bytes = section(&mut input, format::MAX_BYTES + format::HEADER_LEN + 16)?;
        let command = headers
            .split(|b| *b == b'\n')
            .find_map(|line| line.strip_prefix(b"command="))
            .context("missing filter command")?;
        let result = match command {
            b"clean" => transform(repo, true, &bytes),
            b"smudge" => transform(repo, false, &bytes),
            _ => bail!("unsupported Git filter command"),
        };
        match result {
            Ok(bytes) => {
                write_packet(&mut output, b"status=success\n")?;
                output.write_all(b"0000")?;
                write_packet(&mut output, &bytes)?;
                output.write_all(b"00000000")?;
            }
            Err(error) => {
                eprintln!("over: {error:#}");
                write_packet(&mut output, b"status=error\n")?;
                output.write_all(b"0000")?;
            }
        }
        output.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growing_secret_buffers_wipes_the_retired_allocation_before_releasing_it() {
        let mut bytes = Zeroizing::new(Vec::with_capacity(4));
        bytes.extend_from_slice(b"\xffkey");
        let original = bytes.as_ptr();
        let retired = reserve_secret(&mut bytes, 9).unwrap();
        assert_eq!(retired.as_ptr(), original);
        assert_eq!(retired.as_slice(), &[0; 4]);
        assert_eq!(bytes.as_slice(), b"\xffkey");
        assert!(bytes.capacity() >= 9);
        assert_ne!(bytes.as_ptr(), original);
        let replacement = bytes.as_ptr();
        assert!(reserve_secret(&mut bytes, 8).is_none());
        assert_eq!(bytes.as_ptr(), replacement);
        append_secret(&mut bytes, b"\0more", 9).unwrap();
        assert_eq!(bytes.as_slice(), b"\xffkey\0more");
        assert!(append_secret(&mut bytes, b"!", 9).is_err());
        assert_eq!(bytes.as_slice(), b"\xffkey\0more");
    }

    #[test]
    fn bounded_single_shot_and_packet_reads_preserve_binary_bytes_across_growth() {
        let expected: Vec<u8> = (0..200_000).map(|index| index as u8).collect();
        assert_eq!(
            read_input(&mut expected.as_slice(), expected.len())
                .unwrap()
                .as_slice(),
            expected
        );
        assert!(read_input(&mut expected.as_slice(), expected.len() - 1).is_err());
        assert!(read_input(&mut b"x".as_slice(), 0).is_err());
        assert!(read_input(&mut b"".as_slice(), 0).unwrap().is_empty());
        let mut packets = Vec::new();
        write_packet(&mut packets, &expected).unwrap();
        packets.extend_from_slice(b"0000");
        assert_eq!(
            section(&mut packets.as_slice(), expected.len())
                .unwrap()
                .as_slice(),
            expected
        );
        assert!(section(&mut packets.as_slice(), expected.len() - 1).is_err());
        let mut packet_input = packets.as_slice();
        let Packet::Data(first) = packet(&mut packet_input).unwrap() else {
            panic!("expected data")
        };
        // The type assertion prevents regressing packet-owned plaintext to an ordinary Vec.
        let first: Zeroizing<Vec<u8>> = first;
        assert_eq!(first.len(), 65516);
    }

    fn repository(unlocked: bool) -> (tempfile::TempDir, git2::Repository, format::RepositoryKey) {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        let key = format::RepositoryKey::decode(&[42; 96]).unwrap();
        if unlocked {
            let registry = access::Registry {
                version: 1,
                repository: key.repository,
                generation: key.generation,
                users: [(
                    "user".into(),
                    [(
                        "key".into(),
                        access::AccessKey {
                            recipient: None,
                            envelope: vec![],
                        },
                    )]
                    .into(),
                )]
                .into(),
            };
            access::save(&repo, &registry).unwrap();
            access::install(&repo, &key).unwrap();
        }
        (directory, repo, key)
    }

    fn handshake() -> Vec<u8> {
        let mut bytes = Vec::new();
        for section in [
            b"git-filter-client\nversion=2\n".as_slice(),
            b"capability=clean\ncapability=smudge\n",
        ] {
            write_packet(&mut bytes, section).unwrap();
            bytes.extend_from_slice(b"0000");
        }
        bytes
    }

    fn request(bytes: &mut Vec<u8>, command: &[u8], content: &[u8]) {
        write_packet(bytes, command).unwrap();
        bytes.extend_from_slice(b"0000");
        write_packet(bytes, content).unwrap();
        bytes.extend_from_slice(b"0000");
    }

    #[test]
    fn only_eof_between_complete_requests_is_a_clean_shutdown() {
        let (_directory, repo, _key) = repository(false);
        let hello = handshake();
        assert!(process_io(&repo, &mut hello.as_slice(), &mut Vec::new()).is_ok());
        for partial in [
            b"0".as_slice(),
            b"00",
            b"000",
            b"0010",
            b"0010command=",
            b"0000",
            b"zzzz",
            b"0003",
            b"fff1",
        ] {
            let mut input = hello.clone();
            input.extend_from_slice(partial);
            assert!(
                process_io(&repo, &mut input.as_slice(), &mut Vec::new()).is_err(),
                "{partial:?}"
            );
        }
        let mut input = hello.clone();
        request(&mut input, b"command=smudge\n", b"binary\0\xff");
        for length in hello.len() + 1..input.len() {
            assert!(
                process_io(&repo, &mut &input[..length], &mut Vec::new()).is_err(),
                "length {length}"
            );
        }
        assert!(process_io(&repo, &mut input.as_slice(), &mut Vec::new()).is_ok());
        // EOF is never a substitute for either handshake section's flush packet.
        for length in 0..hello.len() {
            assert!(
                process_io(&repo, &mut &hello[..length], &mut Vec::new()).is_err(),
                "hello length {length}"
            );
        }
    }

    #[rstest::rstest]
    #[case(0)]
    #[case(1)]
    fn locked_smudge_passes_structural_ciphertext_without_metadata_but_unlocked_authenticates(
        #[case] domain: u8,
    ) {
        let (_locked_directory, locked, key) = repository(false);
        let (_unlocked_directory, unlocked, _) = repository(true);
        let encrypted = key.encrypt(b"secret\0\xff\n", domain).unwrap();
        assert_eq!(
            transform(&locked, false, &encrypted).unwrap().as_slice(),
            encrypted
        );
        assert_eq!(
            transform(&unlocked, true, &encrypted).unwrap().as_slice(),
            encrypted
        );
        let smudged = transform(&unlocked, false, &encrypted).unwrap();
        assert_eq!(
            smudged.as_slice(),
            if domain == 0 {
                b"secret\0\xff\n"
            } else {
                encrypted.as_slice()
            }
        );
        for offset in [format::HEADER_LEN, encrypted.len() - 1] {
            let mut tampered = encrypted.clone();
            tampered[offset] ^= 1;
            assert_eq!(
                transform(&locked, false, &tampered).unwrap().as_slice(),
                tampered
            );
            for clean in [false, true] {
                assert!(transform(&unlocked, clean, &tampered).is_err());
            }
            assert!(transform(&locked, true, &tampered).is_err());
        }
        for length in 1..format::HEADER_LEN + 16 {
            assert!(transform(&locked, false, &encrypted[..length]).is_err());
        }
    }

    #[test]
    fn historical_binary_plaintext_is_preserved_but_locked_clean_rejects_it() {
        let (_directory, repo, _) = repository(false);
        let plaintext = b"historical\0\xff\n";
        assert_eq!(
            transform(&repo, false, plaintext).unwrap().as_slice(),
            plaintext
        );
        assert!(transform(&repo, true, plaintext).is_err());
    }

    #[test]
    fn failed_authentication_emits_only_error_status_and_the_session_can_continue() {
        let (_directory, repo, key) = repository(true);
        let plaintext = b"never emit this secret\0\xff\n";
        let encrypted = key.encrypt(plaintext, 0).unwrap();
        let mut expected_handshake = Vec::new();
        process_io(&repo, &mut handshake().as_slice(), &mut expected_handshake).unwrap();
        for offset in format::HEADER_LEN..encrypted.len() {
            let mut tampered = encrypted.clone();
            tampered[offset] ^= 1;
            let mut input = handshake();
            request(&mut input, b"command=smudge\n", &tampered);
            request(&mut input, b"command=smudge\n", b"historical\0\xff");
            let mut output = Vec::new();
            process_io(&repo, &mut input.as_slice(), &mut output).unwrap();
            let mut expected = expected_handshake.clone();
            write_packet(&mut expected, b"status=error\n").unwrap();
            expected.extend_from_slice(b"0000");
            write_packet(&mut expected, b"status=success\n").unwrap();
            expected.extend_from_slice(b"0000");
            write_packet(&mut expected, b"historical\0\xff").unwrap();
            expected.extend_from_slice(b"00000000");
            assert_eq!(output, expected, "offset {offset}");
        }
    }
}
