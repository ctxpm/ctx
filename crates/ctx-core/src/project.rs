//! `ctx.toml`: which packages a directory wants and where they are installed.
//!
//! ```toml
//! [packages]
//! team-handbook = "^1.0.0"
//!
//! [install]
//! dir = ".ctx/packages"          # where packages land, relative to this file
//! preserve = [".local", "*.local.yml"]   # paths inside a package kept across updates
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Context, Result, err};
use crate::manifest::is_package_name;
use crate::range;

pub const PROJECT_FILE: &str = "ctx.toml";
pub const DEFAULT_INSTALL_DIR: &str = ".ctx/packages";

fn default_dir() -> String {
    DEFAULT_INSTALL_DIR.to_string()
}

fn is_default_install(install: &Install) -> bool {
    install.dir == DEFAULT_INSTALL_DIR && install.preserve.is_empty()
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Install {
    #[serde(default = "default_dir")]
    pub dir: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preserve: Vec<String>,
}

impl Default for Install {
    fn default() -> Self {
        Install {
            dir: default_dir(),
            preserve: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct Project {
    #[serde(default)]
    pub packages: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "is_default_install")]
    pub install: Install,
}

/// A relative path made of plain names only: no "..", no root, no drive.
fn is_contained(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

impl Project {
    /// Reads `ctx.toml` from a directory; a missing file is an empty project.
    pub fn load(dir: &Path) -> Result<Project> {
        let path = dir.join(PROJECT_FILE);
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Project::default());
            }
            Err(error) => return Err(err(format!("cannot read {}: {error}", path.display()))),
        };
        let project: Project =
            toml::from_str(&raw).context(format!("cannot read {}", path.display()))?;
        project.validate().context(path.display())?;
        Ok(project)
    }

    pub fn validate(&self) -> Result<()> {
        for (name, package_range) in &self.packages {
            if !is_package_name(name) {
                return Err(err(format!("\"{name}\" is not a valid package name")));
            }
            range::parse(package_range)?;
        }
        if !is_contained(&self.install.dir) {
            return Err(err(
                "install.dir must be a relative path inside the project, without \"..\"",
            ));
        }
        for pattern in &self.install.preserve {
            if !is_contained(pattern) {
                return Err(err(format!(
                    "install.preserve entry \"{pattern}\" must be a relative path without \"..\""
                )));
            }
        }
        Ok(())
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let path = dir.join(PROJECT_FILE);
        let contents = toml::to_string_pretty(self).context("cannot serialise ctx.toml")?;
        fs::write(&path, contents).context(format!("cannot write {}", path.display()))
    }

    /// Absolute directory packages are installed into.
    pub fn packages_dir(&self, project_dir: &Path) -> PathBuf {
        project_dir.join(&self.install.dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_an_empty_project_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let project = Project::load(dir.path()).unwrap();
        assert!(project.packages.is_empty());
        assert_eq!(project.install.dir, ".ctx/packages");
        assert_eq!(
            project.packages_dir(dir.path()),
            dir.path().join(".ctx/packages")
        );
    }

    #[test]
    fn round_trips_and_omits_default_install_settings() {
        let dir = tempfile::tempdir().unwrap();
        let mut project = Project::default();
        project
            .packages
            .insert("team-handbook".into(), "^1.0.0".into());
        project.save(dir.path()).unwrap();
        let written = fs::read_to_string(dir.path().join(PROJECT_FILE)).unwrap();
        assert!(written.contains("team-handbook = \"^1.0.0\""));
        assert!(!written.contains("[install]"));
        assert_eq!(Project::load(dir.path()).unwrap(), project);
    }

    #[test]
    fn reads_install_settings() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(PROJECT_FILE),
            "[packages]\nbase = \"*\"\n\n[install]\ndir = \"layers\"\npreserve = [\".local\", \"*.local.yml\", \"knowledge/.git\"]\n",
        )
        .unwrap();
        let project = Project::load(dir.path()).unwrap();
        assert_eq!(project.install.dir, "layers");
        assert_eq!(project.install.preserve.len(), 3);
        project.save(dir.path()).unwrap();
        assert_eq!(Project::load(dir.path()).unwrap(), project);
    }

    #[test]
    fn rejects_paths_that_leave_the_project_and_bad_entries() {
        let dir = tempfile::tempdir().unwrap();
        for contents in [
            "[install]\ndir = \"../outside\"\n",
            "[install]\ndir = \"/etc\"\n",
            "[install]\npreserve = [\"../x\"]\n",
            "[packages]\n\"Bad Name\" = \"*\"\n",
            "[packages]\nbase = \"^1 || ^2\"\n",
            "not toml at all [",
        ] {
            fs::write(dir.path().join(PROJECT_FILE), contents).unwrap();
            assert!(Project::load(dir.path()).is_err(), "{contents}");
        }
    }
}
