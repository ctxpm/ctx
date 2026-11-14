//! Putting a downloaded package on disk: verify, unpack next to the destination, carry
//! over local files, then swap directories so a package is never seen half-installed.

use std::fs;
use std::path::{Path, PathBuf};

use crate::archive;
use crate::error::{Context, Result, err};
use crate::manifest::{MANIFEST_FILE, Manifest};

/// Where packages are installed and which paths inside a package survive an update.
#[derive(Debug, Clone)]
pub struct Target {
    pub packages_dir: PathBuf,
    /// Paths relative to a package directory; `*` matches within one path segment.
    pub preserve: Vec<String>,
}

/// Matches one path segment against a pattern in which `*` stands for any run of characters.
fn segment_matches(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some((star_p, star_t)) = star {
            p = star_p + 1;
            t = star_t + 1;
            star = Some((star_p, star_t + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

fn path_matches(pattern: &str, relative: &Path) -> bool {
    let wanted: Vec<&str> = pattern
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    let actual: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    wanted.len() == actual.len()
        && wanted
            .iter()
            .zip(&actual)
            .all(|(pattern, text)| segment_matches(pattern, text))
}

/// Relative paths under `root` that match a preserve pattern. A matching directory is
/// returned as a whole and not searched further.
fn preserved_paths(root: &Path, patterns: &[String]) -> Result<Vec<PathBuf>> {
    fn walk(
        root: &Path,
        dir: &Path,
        patterns: &[String],
        depth: usize,
        found: &mut Vec<PathBuf>,
    ) -> Result<()> {
        let deepest = patterns
            .iter()
            .map(|pattern| pattern.split('/').count())
            .max()
            .unwrap_or(0);
        if depth >= deepest {
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| err("path outside package"))?
                .to_path_buf();
            if patterns
                .iter()
                .any(|pattern| path_matches(pattern, &relative))
            {
                found.push(relative);
            } else if entry.file_type()?.is_dir() {
                walk(root, &path, patterns, depth + 1, found)?;
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    if !patterns.is_empty() && root.is_dir() {
        walk(root, root, patterns, 0, &mut found)?;
    }
    found.sort();
    Ok(found)
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Installs one package. `archive_bytes` must hash to `expected_integrity`; nothing is
/// written before that is checked. An existing installation is replaced as a whole,
/// except for the paths listed in `target.preserve`, which are carried over.
pub fn install(
    target: &Target,
    name: &str,
    version: &str,
    archive_bytes: &[u8],
    expected_integrity: &str,
    archive_prefix: &str,
) -> Result<PathBuf> {
    let actual = archive::integrity(archive_bytes);
    if actual != expected_integrity {
        return Err(err(format!(
            "integrity check failed for {name}@{version}: the registry lists {expected_integrity} but the download is {actual}; nothing was installed"
        )));
    }

    fs::create_dir_all(&target.packages_dir)
        .context(format!("cannot create {}", target.packages_dir.display()))?;
    let dest = target.packages_dir.join(name);
    let staging = target
        .packages_dir
        .join(format!(".{name}.new-{}", std::process::id()));
    let retired = target
        .packages_dir
        .join(format!(".{name}.old-{}", std::process::id()));
    remove_path(&staging)?;
    remove_path(&retired)?;

    let result = (|| -> Result<()> {
        archive::unpack(archive_bytes, archive_prefix, &staging)?;

        // The archive must be the package it was listed as.
        let manifest_path = staging.join(MANIFEST_FILE);
        let manifest = Manifest::parse(
            &fs::read_to_string(&manifest_path)
                .map_err(|_| err(format!("{name}@{version} has no {MANIFEST_FILE}")))?,
        )?;
        if manifest.name != name || manifest.version != version {
            return Err(err(format!(
                "the archive listed as {name}@{version} contains {}@{}; nothing was installed",
                manifest.name, manifest.version
            )));
        }

        for relative in preserved_paths(&dest, &target.preserve)? {
            let to = staging.join(&relative);
            remove_path(&to)?;
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(dest.join(&relative), &to)
                .context(format!("cannot keep {}", relative.display()))?;
        }

        let had_previous = fs::symlink_metadata(&dest).is_ok();
        if had_previous {
            fs::rename(&dest, &retired).context(format!("cannot replace {}", dest.display()))?;
        }
        if let Err(error) = fs::rename(&staging, &dest) {
            if had_previous {
                // Put the previous installation back rather than leave nothing.
                let _ = fs::rename(&retired, &dest);
            }
            return Err(err(format!(
                "cannot install into {}: {error}",
                dest.display()
            )));
        }
        Ok(())
    })();

    let _ = remove_path(&staging);
    let _ = remove_path(&retired);
    result.map(|()| dest)
}

/// Removes an installed package. Paths listed in `target.preserve` are left in place, so
/// local work inside a package directory is never deleted. Returns true when the whole
/// directory is gone.
pub fn uninstall(target: &Target, name: &str) -> Result<bool> {
    let dest = target.packages_dir.join(name);
    if fs::symlink_metadata(&dest).is_err() {
        return Ok(true);
    }
    let keep = preserved_paths(&dest, &target.preserve)?;
    if keep.is_empty() {
        remove_path(&dest).context(format!("cannot remove {}", dest.display()))?;
        return Ok(true);
    }

    fn prune(root: &Path, dir: &Path, keep: &[PathBuf]) -> Result<bool> {
        let mut empty = true;
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| err("path outside package"))?;
            if keep.iter().any(|kept| kept == relative) {
                empty = false;
            } else if entry.file_type()?.is_dir()
                && keep.iter().any(|kept| kept.starts_with(relative))
            {
                if prune(root, &path, keep)? {
                    fs::remove_dir(&path)?;
                } else {
                    empty = false;
                }
            } else {
                remove_path(&path)?;
            }
        }
        Ok(empty)
    }
    prune(&dest, &dest, &keep)?;
    Ok(false)
}

/// The version of the package installed under `name`, if there is one.
pub fn installed_version(target: &Target, name: &str) -> Option<String> {
    let raw = fs::read_to_string(target.packages_dir.join(name).join(MANIFEST_FILE)).ok()?;
    Manifest::parse(&raw)
        .ok()
        .filter(|manifest| manifest.name == name)
        .map(|manifest| manifest.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn package(name: &str, version: &str, files: &[(&str, &str)]) -> Vec<u8> {
        let source = tempfile::tempdir().unwrap();
        write(
            source.path(),
            "manifest.json",
            &format!(
                r#"{{"schemaVersion":1,"name":"{name}","version":"{version}","type":"project"}}"#
            ),
        );
        for (path, contents) in files {
            write(source.path(), path, contents);
        }
        archive::pack(source.path()).unwrap().1
    }

    fn target(dir: &Path, preserve: &[&str]) -> Target {
        Target {
            packages_dir: dir.join("packages"),
            preserve: preserve.iter().map(|pattern| pattern.to_string()).collect(),
        }
    }

    fn leftovers(target: &Target) -> Vec<String> {
        fs::read_dir(&target.packages_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with('.'))
            .collect()
    }

    #[test]
    fn installs_and_reports_the_installed_version() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path(), &[]);
        let bytes = package("alpha", "1.0.0", &[("docs/a.md", "a")]);
        let dest = install(
            &target,
            "alpha",
            "1.0.0",
            &bytes,
            &archive::integrity(&bytes),
            "",
        )
        .unwrap();
        assert_eq!(dest, target.packages_dir.join("alpha"));
        assert_eq!(fs::read_to_string(dest.join("docs/a.md")).unwrap(), "a");
        assert_eq!(
            installed_version(&target, "alpha").as_deref(),
            Some("1.0.0")
        );
        assert_eq!(installed_version(&target, "missing"), None);
        assert!(leftovers(&target).is_empty());
    }

    #[test]
    fn an_update_replaces_the_whole_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path(), &[]);
        let old = package(
            "alpha",
            "1.0.0",
            &[("removed-later.md", "old"), ("same.md", "old")],
        );
        install(
            &target,
            "alpha",
            "1.0.0",
            &old,
            &archive::integrity(&old),
            "",
        )
        .unwrap();
        write(&target.packages_dir, "alpha/stray-local-file.md", "local");

        let new = package("alpha", "1.1.0", &[("same.md", "new")]);
        let dest = install(
            &target,
            "alpha",
            "1.1.0",
            &new,
            &archive::integrity(&new),
            "",
        )
        .unwrap();
        assert_eq!(fs::read_to_string(dest.join("same.md")).unwrap(), "new");
        assert!(!dest.join("removed-later.md").exists());
        assert!(!dest.join("stray-local-file.md").exists());
        assert_eq!(
            installed_version(&target, "alpha").as_deref(),
            Some("1.1.0")
        );
        assert!(leftovers(&target).is_empty());
    }

    #[test]
    fn preserved_paths_survive_an_update() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path(), &[".local", "*.local.yml", "knowledge/.git"]);
        let old = package("alpha", "1.0.0", &[("knowledge/notes.md", "old")]);
        install(
            &target,
            "alpha",
            "1.0.0",
            &old,
            &archive::integrity(&old),
            "",
        )
        .unwrap();
        write(&target.packages_dir, "alpha/.local/scratch.md", "mine");
        write(
            &target.packages_dir,
            "alpha/settings.local.yml",
            "mine: true",
        );
        write(&target.packages_dir, "alpha/knowledge/.git/HEAD", "ref");
        write(&target.packages_dir, "alpha/not-preserved.md", "gone");

        // The new version ships a file at a preserved path; the local one wins.
        let new = package(
            "alpha",
            "1.1.0",
            &[
                ("knowledge/notes.md", "new"),
                ("settings.local.yml", "shipped"),
            ],
        );
        let dest = install(
            &target,
            "alpha",
            "1.1.0",
            &new,
            &archive::integrity(&new),
            "",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(dest.join(".local/scratch.md")).unwrap(),
            "mine"
        );
        assert_eq!(
            fs::read_to_string(dest.join("settings.local.yml")).unwrap(),
            "mine: true"
        );
        assert_eq!(
            fs::read_to_string(dest.join("knowledge/.git/HEAD")).unwrap(),
            "ref"
        );
        assert_eq!(
            fs::read_to_string(dest.join("knowledge/notes.md")).unwrap(),
            "new"
        );
        assert!(!dest.join("not-preserved.md").exists());
    }

    #[test]
    fn a_wrong_digest_installs_nothing_and_keeps_the_old_version() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path(), &[]);
        let old = package("alpha", "1.0.0", &[("a.md", "old")]);
        install(
            &target,
            "alpha",
            "1.0.0",
            &old,
            &archive::integrity(&old),
            "",
        )
        .unwrap();

        let new = package("alpha", "1.1.0", &[("a.md", "new")]);
        let mut tampered = new.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;
        let error = install(
            &target,
            "alpha",
            "1.1.0",
            &tampered,
            &archive::integrity(&new),
            "",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("integrity check failed"), "{error}");
        assert_eq!(
            installed_version(&target, "alpha").as_deref(),
            Some("1.0.0")
        );
        assert_eq!(
            fs::read_to_string(target.packages_dir.join("alpha/a.md")).unwrap(),
            "old"
        );
        assert!(leftovers(&target).is_empty());
    }

    #[test]
    fn an_archive_for_another_package_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target = target(dir.path(), &[]);
        let other = package("beta", "1.0.0", &[]);
        let error = install(
            &target,
            "alpha",
            "1.0.0",
            &other,
            &archive::integrity(&other),
            "",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("contains beta@1.0.0"), "{error}");
        assert!(!target.packages_dir.join("alpha").exists());
        assert!(leftovers(&target).is_empty());

        let wrong_version = package("alpha", "2.0.0", &[]);
        assert!(
            install(
                &target,
                "alpha",
                "1.0.0",
                &wrong_version,
                &archive::integrity(&wrong_version),
                ""
            )
            .is_err()
        );
    }

    #[test]
    fn uninstall_removes_the_package_but_keeps_preserved_paths() {
        let dir = tempfile::tempdir().unwrap();
        let plain = target(dir.path(), &[]);
        let bytes = package("alpha", "1.0.0", &[("docs/a.md", "a")]);
        install(
            &plain,
            "alpha",
            "1.0.0",
            &bytes,
            &archive::integrity(&bytes),
            "",
        )
        .unwrap();
        assert!(uninstall(&plain, "alpha").unwrap());
        assert!(!plain.packages_dir.join("alpha").exists());
        assert!(uninstall(&plain, "alpha").unwrap());

        let keeping = target(dir.path(), &["knowledge/.git", ".local"]);
        install(
            &keeping,
            "alpha",
            "1.0.0",
            &bytes,
            &archive::integrity(&bytes),
            "",
        )
        .unwrap();
        write(&keeping.packages_dir, "alpha/knowledge/.git/HEAD", "ref");
        write(&keeping.packages_dir, "alpha/knowledge/notes.md", "notes");
        assert!(!uninstall(&keeping, "alpha").unwrap());
        let root = keeping.packages_dir.join("alpha");
        assert!(root.join("knowledge/.git/HEAD").is_file());
        assert!(!root.join("knowledge/notes.md").exists());
        assert!(!root.join("docs").exists());
        assert!(!root.join("manifest.json").exists());
    }

    #[test]
    fn segment_patterns() {
        assert!(segment_matches("*.local.yml", "settings.local.yml"));
        assert!(segment_matches("*", "anything"));
        assert!(segment_matches("a*c", "abbbc"));
        assert!(segment_matches(".local", ".local"));
        assert!(!segment_matches("*.local.yml", "settings.yml"));
        assert!(!segment_matches(".local", ".locale"));
        assert!(path_matches("knowledge/.git", Path::new("knowledge/.git")));
        assert!(!path_matches("knowledge/.git", Path::new(".git")));
        assert!(!path_matches("*.md", Path::new("docs/a.md")));
    }
}
