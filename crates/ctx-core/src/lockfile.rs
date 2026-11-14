//! `ctx.lock`: the exact versions a project installed, so the next install gets the same.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::error::{Context, Result, err};
use crate::registry::VersionInfo;

pub const LOCK_FILE: &str = "ctx.lock";

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LockedPackage {
    pub version: String,
    #[serde(rename = "type")]
    pub kind: String,
    /// `sha256-<hex>` of the archive; checked before anything is extracted.
    pub integrity: String,
    /// Object name of the archive in the registry's bucket.
    pub artifact: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub archive_prefix: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Lockfile {
    pub lockfile_version: u32,
    #[serde(default)]
    pub packages: BTreeMap<String, LockedPackage>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Lockfile {
            lockfile_version: 1,
            packages: BTreeMap::new(),
        }
    }
}

impl From<&VersionInfo> for LockedPackage {
    fn from(info: &VersionInfo) -> Self {
        LockedPackage {
            version: info.version.to_string(),
            kind: info.kind.clone(),
            integrity: info.integrity.clone(),
            artifact: info.artifact.clone(),
            archive_prefix: info.archive_prefix.clone(),
            dependencies: info.dependencies.clone(),
        }
    }
}

impl Lockfile {
    /// Reads `ctx.lock` from a project directory; a missing file is an empty lockfile.
    pub fn load(dir: &Path) -> Result<Lockfile> {
        let path = dir.join(LOCK_FILE);
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Lockfile::default());
            }
            Err(error) => return Err(err(format!("cannot read {}: {error}", path.display()))),
        };
        let lockfile: Lockfile =
            serde_json::from_str(&raw).context(format!("cannot read {}", path.display()))?;
        if lockfile.lockfile_version != 1 {
            return Err(err(format!(
                "{} has lockfileVersion {}; this version of ctx understands 1",
                path.display(),
                lockfile.lockfile_version
            )));
        }
        Ok(lockfile)
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let path = dir.join(LOCK_FILE);
        let mut contents = serde_json::to_string_pretty(self)?;
        contents.push('\n');
        fs::write(&path, contents).context(format!("cannot write {}", path.display()))
    }

    /// The pinned versions, for the resolver.
    pub fn versions(&self) -> BTreeMap<String, Version> {
        self.packages
            .iter()
            .filter_map(|(name, package)| {
                Some((name.clone(), Version::parse(&package.version).ok()?))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_stays_stable() {
        let dir = tempfile::tempdir().unwrap();
        let mut lockfile = Lockfile::default();
        lockfile.packages.insert(
            "team-handbook".into(),
            LockedPackage {
                version: "1.2.0".into(),
                kind: "team".into(),
                integrity: "sha256-abc".into(),
                artifact: "packages/team-handbook/1.2.0/abc.tgz".into(),
                archive_prefix: String::new(),
                dependencies: BTreeMap::from([(
                    "frontend-standards".to_string(),
                    "^1.0.0".to_string(),
                )]),
            },
        );
        lockfile.save(dir.path()).unwrap();
        let first = fs::read_to_string(dir.path().join(LOCK_FILE)).unwrap();
        assert!(first.ends_with("}\n"));
        assert!(first.contains("\"lockfileVersion\": 1"));
        assert!(!first.contains("archivePrefix"));

        let loaded = Lockfile::load(dir.path()).unwrap();
        assert_eq!(loaded, lockfile);
        assert_eq!(
            loaded.versions().get("team-handbook"),
            Some(&Version::new(1, 2, 0))
        );
        loaded.save(dir.path()).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join(LOCK_FILE)).unwrap(),
            first
        );
    }

    #[test]
    fn missing_file_is_empty_and_unknown_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Lockfile::load(dir.path()).unwrap(), Lockfile::default());
        fs::write(
            dir.path().join(LOCK_FILE),
            r#"{"lockfileVersion":2,"packages":{}}"#,
        )
        .unwrap();
        assert!(
            Lockfile::load(dir.path())
                .unwrap_err()
                .to_string()
                .contains("lockfileVersion 2")
        );
        fs::write(dir.path().join(LOCK_FILE), "garbage").unwrap();
        assert!(Lockfile::load(dir.path()).is_err());
    }
}
