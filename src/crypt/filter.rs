//! Git protocol plumbing never prints presentation output or prompts.

use std::io::{Read, Write};

use anyhow::{Context as _, Result, bail, ensure};

use super::{access, format};

pub fn transform(repo: &git2::Repository, clean: bool, input: &[u8]) -> Result<Vec<u8>> {
    let _guard = access::guard(repo, false)?;
    if format::recognized(input) {
        format::inspect(input)?;
        // A locked checkout may visit blobs before checking out their access metadata.
        if !clean && access::local_key_missing(repo)? {
            return Ok(input.to_vec());
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
                    Ok(input.to_vec())
                } else {
                    Ok(plaintext.to_vec())
                }
            }
            Err(error) => {
                if access::local_key_missing(repo)? {
                    Ok(input.to_vec())
                } else {
                    Err(error)
                }
            }
        }
    } else if clean {
        access::local_key(repo)?.encrypt(input, 0)
    } else {
        // Historical plaintext is preserved for recovery; doctor identifies exposure.
        eprintln!(
            "over: protected Git blob is plaintext; run over doctor to inspect stored exposure"
        );
        Ok(input.to_vec())
    }
}

pub fn once(clean: bool) -> Result<()> {
    let repo = git2::Repository::open_from_env().or_else(|_| git2::Repository::discover("."))?;
    let mut input = zeroize::Zeroizing::new(Vec::new());
    std::io::stdin()
        .take((format::MAX_BYTES + format::HEADER_LEN + 17) as u64)
        .read_to_end(&mut input)?;
    ensure!(
        input.len() <= format::MAX_BYTES + format::HEADER_LEN + 16,
        "filter input exceeds 256 MiB limit"
    );
    let output = zeroize::Zeroizing::new(transform(&repo, clean, &input)?);
    std::io::stdout().write_all(&output)?;
    Ok(())
}

fn packet(input: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let mut length = [0; 4];
    input.read_exact(&mut length)?;
    let length = usize::from_str_radix(std::str::from_utf8(&length)?, 16)?;
    if length == 0 {
        return Ok(None);
    }
    ensure!((4..=65520).contains(&length), "invalid Git packet length");
    let mut bytes = vec![0; length - 4];
    input.read_exact(&mut bytes)?;
    Ok(Some(bytes))
}

fn section(input: &mut impl Read, limit: usize) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    while let Some(packet) = packet(input)? {
        ensure!(
            bytes.len() + packet.len() <= limit,
            "Git filter section exceeds size limit"
        );
        bytes.extend_from_slice(&packet);
    }
    Ok(bytes)
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
        let first = match packet(&mut input) {
            Ok(Some(first)) => first,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::UnexpectedEof) =>
            {
                return Ok(());
            }
            Ok(None) => bail!("missing filter request"),
            Err(error) => return Err(error),
        };
        let rest = section(&mut input, 64 * 1024)?;
        let mut headers = first;
        headers.extend_from_slice(&rest);
        let bytes = section(&mut input, format::MAX_BYTES + format::HEADER_LEN + 16)?;
        let command = headers
            .split(|b| *b == b'\n')
            .find_map(|line| line.strip_prefix(b"command="))
            .context("missing filter command")?;
        let result = match command {
            b"clean" => transform(&repo, true, &bytes),
            b"smudge" => transform(&repo, false, &bytes),
            _ => bail!("unsupported Git filter command"),
        };
        match result {
            Ok(bytes) => {
                let bytes = zeroize::Zeroizing::new(bytes);
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
