//! Per-user state: which registries are known and the sign-in session for each.
//!
//! Lives in `$CTX_CONFIG_DIR`, else `$XDG_CONFIG_HOME/ctx`, else `~/.config/ctx`
//! (`%APPDATA%\ctx` on Windows).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::descriptor::Descriptor;
use crate::error::{Context, Result, err};

const CONFIG_FILE: &str = "config.json";
const CREDENTIALS_FILE: &str = "credentials.json";

/// A registry the user added with `ctx registry add`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistryEntry {
    /// Where the descriptor came from: a URL or a file path.
    pub source: String,
    pub descriptor: Descriptor,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_registry: Option<String>,
    #[serde(default)]
    pub registries: BTreeMap<String, RegistryEntry>,
}

/// A signed-in session for one registry.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Session {
    pub email: String,
    pub refresh_token: String,
    pub id_token: String,
    /// Unix time in seconds at which `id_token` expires.
    pub expires_at: u64,
}

pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CTX_CONFIG_DIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(dir).join("ctx"));
    }
    if cfg!(windows) {
        if let Some(dir) = std::env::var_os("APPDATA").filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(dir).join("ctx"));
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| err("cannot find your home directory; set CTX_CONFIG_DIR"))?;
    Ok(PathBuf::from(home).join(".config").join("ctx"))
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> Result<T> {
    match fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw).context(format!("cannot read {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(err(format!("cannot read {}: {error}", path.display()))),
    }
}

/// Writes a file so that it is never seen half-written, readable by the owner only.
fn write_private(path: &Path, contents: &str) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| err("config path has no parent directory"))?;
    fs::create_dir_all(dir).context(format!("cannot create {}", dir.display()))?;
    let temporary = path.with_extension("tmp");
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options
            .open(&temporary)
            .context(format!("cannot write {}", temporary.display()))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&temporary, path).context(format!("cannot write {}", path.display()))?;
    Ok(())
}

impl Config {
    pub fn load() -> Result<Config> {
        read_json(&config_dir()?.join(CONFIG_FILE))
    }

    pub fn save(&self) -> Result<()> {
        write_private(
            &config_dir()?.join(CONFIG_FILE),
            &serde_json::to_string_pretty(self)?,
        )
    }

    /// The registry to use: the named one, else the default, else the only one.
    pub fn registry(&self, name: Option<&str>) -> Result<(String, &RegistryEntry)> {
        let name = match name.or(self.default_registry.as_deref()) {
            Some(name) => name.to_string(),
            None if self.registries.len() == 1 => {
                self.registries.keys().next().cloned().unwrap_or_default()
            }
            None if self.registries.is_empty() => {
                return Err(err(
                    "no registry configured; add one with `ctx registry add <url>`",
                ));
            }
            None => {
                return Err(err(
                    "several registries are configured; pick one with --registry or `ctx registry use`",
                ));
            }
        };
        let entry = self.registries.get(&name).ok_or_else(|| {
            err(format!(
                "unknown registry \"{name}\"; see `ctx registry list`"
            ))
        })?;
        Ok((name, entry))
    }
}

/// Sign-in sessions, keyed by registry name. Stored in a file only the owner can read.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Credentials {
    #[serde(default)]
    pub sessions: BTreeMap<String, Session>,
}

impl Credentials {
    pub fn load() -> Result<Credentials> {
        read_json(&config_dir()?.join(CREDENTIALS_FILE))
    }

    pub fn save(&self) -> Result<()> {
        write_private(
            &config_dir()?.join(CREDENTIALS_FILE),
            &serde_json::to_string_pretty(self)?,
        )
    }
}
