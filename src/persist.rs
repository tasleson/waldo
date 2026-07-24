// SPDX-License-Identifier: MIT
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The status the remote side was last successfully told via webhook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportedStatus {
    Online,
    Offline,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedState {
    last_reported: Option<ReportedStatus>,
}

/// Durable record of the last delivered webhook status, so waldo can
/// reconcile with the remote side after a restart or reboot.
pub struct StateFile {
    path: PathBuf,
    state: PersistedState,
}

impl StateFile {
    /// `$XDG_STATE_HOME/waldo/state.toml`, defaulting to
    /// `~/.local/state/waldo/state.toml`.
    pub fn default_path() -> PathBuf {
        let base = std::env::var("XDG_STATE_HOME")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                PathBuf::from(home).join(".local/state")
            });
        base.join("waldo/state.toml")
    }

    /// A missing or unreadable file is not an error: it simply means the last
    /// reported status is unknown.
    pub fn load(path: PathBuf) -> Self {
        let state = match std::fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str(&contents) {
                Ok(state) => state,
                Err(e) => {
                    tracing::warn!("Ignoring malformed state file {}: {e}", path.display());
                    PersistedState::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => PersistedState::default(),
            Err(e) => {
                tracing::warn!("Failed to read state file {}: {e}", path.display());
                PersistedState::default()
            }
        };
        Self { path, state }
    }

    pub fn last_reported(&self) -> Option<ReportedStatus> {
        self.state.last_reported
    }

    /// Record a successfully delivered webhook status and persist it to disk.
    pub fn record(&mut self, status: ReportedStatus) {
        self.state.last_reported = Some(status);
        if let Err(e) = self.save() {
            tracing::error!("Failed to persist state to {}: {e:#}", self.path.display());
        }
    }

    fn save(&self) -> anyhow::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let contents = toml::to_string(&self.state)?;
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, contents)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_means_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateFile::load(dir.path().join("state.toml"));
        assert_eq!(state.last_reported(), None);
    }

    #[test]
    fn malformed_file_means_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        std::fs::write(&path, "last_reported = {{{{").unwrap();
        assert_eq!(StateFile::load(path).last_reported(), None);
    }

    #[test]
    fn record_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");

        for status in [ReportedStatus::Offline, ReportedStatus::Online] {
            StateFile::load(path.clone()).record(status);
            assert_eq!(StateFile::load(path.clone()).last_reported(), Some(status));
        }
    }
}
