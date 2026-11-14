//! Packing a directory into a package archive and unpacking one safely.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};

use crate::error::{Context, Result, err};
use crate::manifest::{MANIFEST_FILE, Manifest};

/// Never packed, wherever they appear.
const ALWAYS_EXCLUDED: &[&str] = &[".git", ".ctx", "node_modules", ".DS_Store"];
const MAX_ENTRIES: usize = 10_000;
const MAX_UNPACKED_BYTES: u64 = 200 * 1024 * 1024;

/// `sha256-<hex>` of some bytes, the form the registry records.
pub fn integrity(bytes: &[u8]) -> String {
    format!("sha256-{}", hex::encode(Sha256::digest(bytes)))
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)
        .context(format!("cannot read {}", dir.display()))?
        .collect::<std::io::Result<_>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        if ALWAYS_EXCLUDED.iter().any(|excluded| name == *excluded) {
            continue;
        }
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(err(format!(
                "{} is a symbolic link; packages may only contain regular files",
                path.strip_prefix(root).unwrap_or(&path).display()
            )));
        }
        if kind.is_dir() {
            collect(root, &path, files)?;
        } else if kind.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

/// Packs a package directory into a `.tgz`. The result depends only on file names and
/// contents, so packing the same directory twice gives the same bytes.
pub fn pack(dir: &Path) -> Result<(Manifest, Vec<u8>)> {
    let manifest_path = dir.join(MANIFEST_FILE);
    let manifest = Manifest::parse(
        &fs::read_to_string(&manifest_path)
            .context(format!("cannot read {}", manifest_path.display()))?,
    )?;

    let mut files = Vec::new();
    collect(dir, dir, &mut files)?;
    if files.len() > MAX_ENTRIES {
        return Err(err(format!("package has more than {MAX_ENTRIES} files")));
    }

    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    for path in &files {
        let relative = path
            .strip_prefix(dir)
            .map_err(|_| err("file outside the package directory"))?;
        let name = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        let contents = fs::read(path).context(format!("cannot read {}", path.display()))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, &name, contents.as_slice())
            .context(format!("cannot pack {name}"))?;
    }
    let bytes = builder
        .into_inner()
        .context("cannot finish archive")?
        .finish()
        .context("cannot finish archive")?;
    Ok((manifest, bytes))
}

/// The path of an archive entry below `prefix`, or an error when it could escape the
/// destination. Returns `None` for the prefix directory itself.
fn entry_path(raw: &Path, prefix: &str) -> Result<Option<PathBuf>> {
    let mut clean = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            _ => return Err(err(format!("unsafe path in archive: {}", raw.display()))),
        }
    }
    if prefix.is_empty() {
        return Ok(if clean.as_os_str().is_empty() {
            None
        } else {
            Some(clean)
        });
    }
    let prefix = Path::new(prefix.trim_end_matches('/'));
    match clean.strip_prefix(prefix) {
        Ok(rest) if rest.as_os_str().is_empty() => Ok(None),
        Ok(rest) => Ok(Some(rest.to_path_buf())),
        Err(_) => Err(err(format!(
            "archive entry {} is outside its top-level directory",
            raw.display()
        ))),
    }
}

/// Unpacks an archive into `dest`, which must not exist yet. Only regular files and
/// directories are accepted; anything that could write outside `dest` is refused.
/// `prefix` is the top-level directory to strip ("" for none). Returns the file count.
pub fn unpack(bytes: &[u8], prefix: &str, dest: &Path) -> Result<usize> {
    fs::create_dir_all(dest).context(format!("cannot create {}", dest.display()))?;
    let mut archive = tar::Archive::new(GzDecoder::new(bytes));
    let mut count = 0usize;
    let mut total = 0u64;

    for entry in archive.entries().context("not a readable .tgz archive")? {
        let mut entry = entry.context("not a readable .tgz archive")?;
        let kind = entry.header().entry_type();
        let raw = entry
            .path()
            .context("archive entry has an unreadable path")?
            .into_owned();
        // Extended headers describe the next entry and carry no content of their own.
        if matches!(
            kind,
            tar::EntryType::XGlobalHeader | tar::EntryType::XHeader
        ) {
            continue;
        }
        if !matches!(kind, tar::EntryType::Regular | tar::EntryType::Directory) {
            return Err(err(format!(
                "archive entry {} is not a regular file or directory; refusing to install it",
                raw.display()
            )));
        }
        let Some(relative) = entry_path(&raw, prefix)? else {
            continue;
        };
        let target = dest.join(&relative);

        if kind == tar::EntryType::Directory {
            fs::create_dir_all(&target)?;
            continue;
        }
        count += 1;
        if count > MAX_ENTRIES {
            return Err(err(format!("archive has more than {MAX_ENTRIES} files")));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        // create_new: never follow or overwrite something an earlier entry put there.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .context(format!("cannot write {}", relative.display()))?;
        let remaining = MAX_UNPACKED_BYTES - total;
        let written = std::io::copy(&mut entry.by_ref().take(remaining + 1), &mut file)?;
        total += written;
        if total > MAX_UNPACKED_BYTES {
            return Err(err(format!(
                "archive unpacks to more than {MAX_UNPACKED_BYTES} bytes"
            )));
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn sample(root: &Path) {
        write(
            root,
            "manifest.json",
            r#"{"schemaVersion":1,"name":"sample","version":"1.0.0","type":"project"}"#,
        );
        write(root, "README.md", "# sample\n");
        write(root, "guides/review.md", "review\n");
    }

    /// Builds an archive with arbitrary entries, including ones `pack` would never produce.
    fn raw_archive(entries: &[(&str, tar::EntryType, &str)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
        for (name, kind, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_mode(0o644);
            if *kind == tar::EntryType::Symlink {
                header.set_size(0);
                header.set_link_name(contents).unwrap();
                // set_path refuses "..", so write the name bytes directly.
                header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
                header.set_cksum();
                builder.append(&header, std::io::empty()).unwrap();
            } else {
                header.set_size(contents.len() as u64);
                header.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
                header.set_cksum();
                builder.append(&header, contents.as_bytes()).unwrap();
            }
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn packs_and_unpacks_a_directory() {
        let source = tempfile::tempdir().unwrap();
        sample(source.path());
        let (manifest, bytes) = pack(source.path()).unwrap();
        assert_eq!(manifest.name, "sample");

        let out = tempfile::tempdir().unwrap();
        let dest = out.path().join("sample");
        assert_eq!(unpack(&bytes, "", &dest).unwrap(), 3);
        assert_eq!(
            fs::read_to_string(dest.join("guides/review.md")).unwrap(),
            "review\n"
        );
        assert_eq!(
            fs::read_to_string(dest.join("README.md")).unwrap(),
            "# sample\n"
        );
    }

    #[test]
    fn packing_is_reproducible_and_skips_excluded_names() {
        let source = tempfile::tempdir().unwrap();
        sample(source.path());
        let (_, first) = pack(source.path()).unwrap();

        write(source.path(), ".git/config", "secret");
        write(source.path(), "node_modules/x/index.js", "x");
        write(source.path(), "guides/.DS_Store", "x");
        let (_, second) = pack(source.path()).unwrap();
        assert_eq!(integrity(&first), integrity(&second));
        assert!(integrity(&first).starts_with("sha256-") && integrity(&first).len() == 7 + 64);
    }

    #[test]
    fn packing_needs_a_valid_manifest() {
        let source = tempfile::tempdir().unwrap();
        write(source.path(), "README.md", "x");
        assert!(
            pack(source.path())
                .unwrap_err()
                .to_string()
                .contains("manifest.json")
        );
        write(
            source.path(),
            "manifest.json",
            r#"{"schemaVersion":1,"name":"Bad","version":"1","type":"x"}"#,
        );
        assert!(
            pack(source.path())
                .unwrap_err()
                .to_string()
                .contains("invalid manifest.json")
        );
    }

    #[cfg(unix)]
    #[test]
    fn packing_refuses_symbolic_links() {
        let source = tempfile::tempdir().unwrap();
        sample(source.path());
        std::os::unix::fs::symlink("/etc/hosts", source.path().join("link")).unwrap();
        assert!(
            pack(source.path())
                .unwrap_err()
                .to_string()
                .contains("symbolic link")
        );
    }

    #[test]
    fn strips_a_top_level_directory() {
        let bytes = raw_archive(&[
            ("package/manifest.json", tar::EntryType::Regular, "{}"),
            ("package/docs/a.md", tar::EntryType::Regular, "a"),
        ]);
        let out = tempfile::tempdir().unwrap();
        let dest = out.path().join("p");
        assert_eq!(unpack(&bytes, "package/", &dest).unwrap(), 2);
        assert!(dest.join("manifest.json").is_file());
        assert!(dest.join("docs/a.md").is_file());

        let stray = raw_archive(&[("other/a.md", tar::EntryType::Regular, "a")]);
        assert!(
            unpack(&stray, "package/", &out.path().join("q"))
                .unwrap_err()
                .to_string()
                .contains("outside its top-level")
        );
    }

    #[test]
    fn refuses_entries_that_could_escape_the_destination() {
        let out = tempfile::tempdir().unwrap();
        for (index, name) in ["../escape.md", "docs/../../escape.md", "/abs.md"]
            .iter()
            .enumerate()
        {
            let bytes = raw_archive(&[(name, tar::EntryType::Regular, "x")]);
            let error = unpack(&bytes, "", &out.path().join(format!("d{index}")))
                .unwrap_err()
                .to_string();
            assert!(error.contains("unsafe path"), "{name}: {error}");
        }
        assert!(!out.path().join("escape.md").exists());
        assert!(!out.path().parent().unwrap().join("escape.md").exists());
    }

    #[test]
    fn refuses_links_and_duplicate_paths() {
        let out = tempfile::tempdir().unwrap();
        let link = raw_archive(&[("link", tar::EntryType::Symlink, "/etc/passwd")]);
        assert!(
            unpack(&link, "", &out.path().join("a"))
                .unwrap_err()
                .to_string()
                .contains("not a regular file")
        );

        let duplicate = raw_archive(&[
            ("a.md", tar::EntryType::Regular, "one"),
            ("a.md", tar::EntryType::Regular, "two"),
        ]);
        assert!(unpack(&duplicate, "", &out.path().join("b")).is_err());
    }

    #[test]
    fn refuses_data_that_is_not_an_archive() {
        let out = tempfile::tempdir().unwrap();
        assert!(unpack(b"not an archive", "", &out.path().join("a")).is_err());
    }
}
