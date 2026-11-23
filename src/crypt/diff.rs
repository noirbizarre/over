//! Git supplies logical paths separately from temporary blob files for name authentication.

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;

use anyhow::{Context as _, Result, ensure};
use zeroize::Zeroizing;

use super::{access, buffer, context, format};

fn value(
    key: Option<&format::RepositoryKey>,
    path: &Path,
    file: &Path,
    mode: &OsString,
) -> Result<Zeroizing<Vec<u8>>> {
    let name = context::secret_name(path)?;
    if mode == "." || mode == "000000" {
        return Ok(Zeroizing::new(Vec::new()));
    }
    ensure!(
        mode == "100644" || mode == "100755",
        "native secret diff requires regular files"
    );
    let input =
        buffer::read_file_with_limit(file, Some(format::MAX_BYTES + format::HEADER_LEN + 16))?;
    if !format::recognized(&input) {
        return Ok(input);
    }
    format::inspect(&input)?;
    ensure!(
        matches!(format::file_domain(&input)?, 1 | 2),
        "unsupported native secret domain"
    );
    if let Some(key) = key {
        let value = crate::secrets::entry::decode(key, &name, &input)?;
        Ok(Zeroizing::new(value.expose().to_vec()))
    } else {
        // Ciphertext remains opaque while locked; never force it through a text renderer.
        Ok(input)
    }
}

fn patch(
    old: &[u8],
    old_path: &Path,
    old_mode: &str,
    new: &[u8],
    new_path: &Path,
    new_mode: &str,
) -> Result<Zeroizing<Vec<u8>>> {
    let mut output = Zeroizing::new(Vec::new());
    let old_present = !matches!(old_mode, "." | "000000");
    let new_present = !matches!(new_mode, "." | "000000");
    if old == new && old_path == new_path && old_mode == new_mode {
        return Ok(output);
    }
    writeln!(
        output,
        "diff --git a/{} b/{}",
        old_path.display(),
        new_path.display()
    )?;
    if !old_present {
        writeln!(output, "new file mode {new_mode}")?;
    } else if !new_present {
        writeln!(output, "deleted file mode {old_mode}")?;
    } else if old_mode != new_mode {
        writeln!(output, "old mode {old_mode}\nnew mode {new_mode}")?;
    }
    if old_path != new_path {
        writeln!(
            output,
            "rename from {}\nrename to {}",
            old_path.display(),
            new_path.display()
        )?;
    }
    if old == new {
        return Ok(output);
    }
    let mut options = git2::DiffOptions::new();
    let mut patch =
        git2::Patch::from_buffers(old, Some(old_path), new, Some(new_path), Some(&mut options))?;
    let old_label = if old_present {
        format!("a/{}", old_path.display())
    } else {
        "/dev/null".into()
    };
    let new_label = if new_present {
        format!("b/{}", new_path.display())
    } else {
        "/dev/null".into()
    };
    if patch.delta().flags().contains(git2::DiffFlags::BINARY) {
        writeln!(output, "Binary files {old_label} and {new_label} differ")?;
        return Ok(output);
    }
    writeln!(output, "--- {old_label}\n+++ {new_label}")?;
    patch.print(&mut |_, _, line| {
        // libgit2's file header contains plaintext-derived object IDs; supply our own.
        if line.origin() == 'F' {
            return true;
        }
        let required = output.len() + line.content().len() + 1;
        drop(buffer::reserve_secret(&mut output, required));
        if matches!(line.origin(), '+' | '-' | ' ') {
            output.push(line.origin() as u8);
        }
        output.extend_from_slice(line.content());
        true
    })?;
    Ok(output)
}

pub fn run(args: &[OsString]) -> Result<()> {
    ensure!(
        matches!(args.len(), 1 | 7 | 9),
        "invalid Git external diff arguments"
    );
    let old_path = Path::new(&args[0]);
    context::secret_name(old_path)
        .context("invalid old native-secret diff path; use --no-relative")?;
    if args.len() == 1 {
        println!("Unmerged native secret {}", old_path.display());
        return Ok(());
    }
    let new_path = if args.len() == 9 {
        Path::new(&args[7])
    } else {
        old_path
    };
    context::secret_name(new_path)
        .context("invalid new native-secret diff path; use --no-relative")?;
    let repo = git2::Repository::open_from_env().or_else(|_| git2::Repository::discover("."))?;
    let _guard = access::guard(&repo, false)?;
    let key = if access::local_key_missing(&repo)? {
        None
    } else {
        Some(access::local_key(&repo)?)
    };
    // Authenticate both sides before constructing or writing any plaintext patch.
    let old = value(key.as_ref(), old_path, Path::new(&args[1]), &args[3])
        .context("could not authenticate old native secret")?;
    let new = value(key.as_ref(), new_path, Path::new(&args[4]), &args[6])
        .context("could not authenticate new native secret")?;
    let output = patch(
        &old,
        old_path,
        args[3].to_str().context("invalid old mode")?,
        &new,
        new_path,
        args[6].to_str().context("invalid new mode")?,
    )?;
    std::io::stdout().lock().write_all(&output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_additions_and_deletions_retain_their_file_identity() {
        let path = Path::new(".over/secrets/token");
        let added = patch(b"", path, ".", b"", path, "100644").unwrap();
        assert!(String::from_utf8_lossy(&added).contains("new file mode 100644"));
        let removed = patch(b"", path, "100644", b"", path, ".").unwrap();
        assert!(String::from_utf8_lossy(&removed).contains("deleted file mode 100644"));
    }

    #[test]
    fn binary_values_are_summarized_and_text_has_no_plaintext_object_ids() {
        let path = Path::new(".over/secrets/token");
        let binary = patch(b"a\0b", path, "100644", b"c\0d", path, "100644").unwrap();
        assert!(String::from_utf8_lossy(&binary).contains("Binary files"));
        assert!(!binary.windows(3).any(|bytes| bytes == b"a\0b"));
        let text = patch(b"before", path, "100644", b"after", path, "100644").unwrap();
        let text = String::from_utf8_lossy(&text);
        assert!(text.contains("-before\n"));
        assert!(text.contains("+after\n"));
        assert!(text.contains("No newline at end of file"));
        assert!(!text.contains("index "));
    }
}
