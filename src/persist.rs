use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::ModelSelection;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PersistedState {
    #[serde(default)]
    pub connection: ConnectionSettings,
    #[serde(default)]
    pub servers: HashMap<String, ServerState>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ConnectionSettings {
    pub server: String,
    pub username: String,
    #[serde(default)]
    pub cloudflare_access: bool,
}

impl Default for ConnectionSettings {
    fn default() -> Self {
        Self {
            server: "http://127.0.0.1:4096".into(),
            username: "opencode".into(),
            cloudflare_access: false,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ServerState {
    #[serde(default)]
    pub tabs: Vec<PersistedTab>,
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub selections: HashMap<String, ModelSelection>,
    #[serde(default)]
    pub unread: HashSet<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PersistedTab {
    pub id: String,
    pub directory: String,
    pub title: String,
}

impl PersistedState {
    pub fn load(path: &Path) -> Result<(Self, Option<String>)> {
        let contents = match fs::read(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Self::default(), None));
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()))
            }
        };
        match serde_json::from_slice(&contents) {
            Ok(state) => Ok((state, None)),
            Err(error) => {
                let backup = path.with_extension(format!("json.corrupt.{}", std::process::id()));
                fs::rename(path, &backup).with_context(|| {
                    format!(
                        "state is invalid ({error}) and could not be moved to {}",
                        backup.display()
                    )
                })?;
                Ok((
                    Self::default(),
                    Some(format!(
                        "Invalid state was preserved at {}",
                        backup.display()
                    )),
                ))
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .context("state path does not have a parent directory")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("failed to secure {}", parent.display()))?;
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = parent.join(format!(".state.json.{}.{}.tmp", std::process::id(), nonce));
        let contents = serde_json::to_vec_pretty(self).context("failed to serialize state")?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("failed to create {}", temporary.display()))?;
        file.write_all(&contents)
            .with_context(|| format!("failed to write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temporary.display()))?;
        fs::rename(&temporary, path)
            .with_context(|| format!("failed to replace {}", path.display()))?;
        Ok(())
    }
}

pub fn default_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("opencode-gtk")
        .join("state.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_without_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/state.json");
        let mut state = PersistedState {
            connection: ConnectionSettings {
                server: "https://opencode.example.com".into(),
                username: "danny".into(),
                cloudflare_access: true,
            },
            ..PersistedState::default()
        };
        state.servers.insert(
            "http://127.0.0.1:4096".into(),
            ServerState {
                tabs: vec![PersistedTab {
                    id: "ses_1".into(),
                    directory: "/repo".into(),
                    title: "Test".into(),
                }],
                active: Some("ses_1".into()),
                selections: HashMap::new(),
                unread: HashSet::from(["ses_1".into()]),
            },
        );

        state.save(&path).unwrap();
        let (loaded, warning) = PersistedState::load(&path).unwrap();

        assert!(warning.is_none());
        assert_eq!(loaded.servers["http://127.0.0.1:4096"].tabs[0].id, "ses_1");
        assert!(loaded.servers["http://127.0.0.1:4096"]
            .unread
            .contains("ses_1"));
        assert_eq!(loaded.connection, state.connection);
        let contents = fs::read_to_string(path).unwrap();
        assert!(!contents.contains("password"));
        assert!(!contents.contains("client.access"));
        assert!(!contents.contains("theme"));
    }

    #[test]
    fn older_state_gets_connection_defaults_and_ignores_legacy_theme() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        fs::write(&path, br#"{"theme":"light","servers":{}}"#).unwrap();

        let (loaded, warning) = PersistedState::load(&path).unwrap();

        assert!(warning.is_none());
        assert_eq!(loaded.connection, ConnectionSettings::default());
    }

    #[test]
    fn invalid_state_is_backed_up_before_reset() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        fs::write(&path, b"not json").unwrap();

        let (loaded, warning) = PersistedState::load(&path).unwrap();

        assert!(loaded.servers.is_empty());
        assert!(warning.is_some());
        assert!(!path.exists());
        assert!(path
            .with_extension(format!("json.corrupt.{}", std::process::id()))
            .exists());
    }

    #[cfg(unix)]
    #[test]
    fn state_file_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        PersistedState::default().save(&path).unwrap();

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(directory.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
