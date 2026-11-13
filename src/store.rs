//! Storage for the account and call history. The password lives in the keychain, the rest in a JSON file.

use crate::i18n::Lang;
use crate::model::{Account, HistoryEntry};
use crate::settings::{AudioSettings, CallSettings, ConnectionSettings};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const KEYCHAIN_SERVICE: &str = "RustSIPPhone";
const HISTORY_LIMIT: usize = 50;

#[derive(Default, Serialize, Deserialize)]
pub struct Stored {
    #[serde(default)]
    pub server: String,
    #[serde(default)]
    pub extension: String,
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
    /// Interface language. Missing in older files, which then default to English.
    #[serde(default)]
    pub language: Lang,
    /// Connection options of the account (transport, domain, proxy and so on).
    #[serde(default)]
    pub connection: ConnectionSettings,
    #[serde(default)]
    pub audio: AudioSettings,
    #[serde(default)]
    pub calls: CallSettings,
}

/// Where the settings file lives. `RUSTSIPPHONE_CONFIG_DIR` overrides the system's location, which
/// makes a portable setup possible and keeps the tests away from the real settings.
fn path() -> Option<PathBuf> {
    let dir = match std::env::var_os("RUSTSIPPHONE_CONFIG_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => dirs::config_dir()?.join("RustSIPPhone"),
    };
    Some(dir.join("settings.json"))
}

pub fn load() -> Stored {
    path().map(|path| load_from(&path)).unwrap_or_default()
}

fn load_from(path: &Path) -> Stored {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Stored::default();
    };
    serde_json::from_str(&text).unwrap_or_else(|_| {
        // The file is there but cannot be read (damaged, or written by a newer version). The
        // next save would replace it with defaults, so keep a copy the user can recover from.
        let _ = std::fs::copy(path, path.with_extension("json.bad"));
        Stored::default()
    })
}

pub fn save(stored: &Stored) {
    if let Some(path) = path() {
        save_to(&path, stored);
    }
}

/// Writes the settings next to the real file and then moves them into place, so a crash or a
/// power cut in the middle leaves the old settings intact instead of half a file.
fn save_to(path: &Path, stored: &Stored) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(text) = serde_json::to_string_pretty(stored) else {
        return;
    };
    let draft = path.with_extension("json.tmp");
    if std::fs::write(&draft, text).is_ok() && std::fs::rename(&draft, path).is_err() {
        let _ = std::fs::remove_file(&draft);
    }
}

pub fn push_history(stored: &mut Stored, entry: HistoryEntry) {
    stored.history.insert(0, entry);
    stored.history.truncate(HISTORY_LIMIT);
}

fn keychain_entry(server: &str, extension: &str) -> Option<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, &format!("{extension}@{server}")).ok()
}

pub fn load_password(server: &str, extension: &str) -> Option<String> {
    keychain_entry(server, extension)?.get_password().ok()
}

pub fn save_password(account: &Account) -> Result<(), String> {
    keychain_entry(&account.server, &account.extension)
        .ok_or("could not open the keychain")?
        .set_password(&account.password)
        .map_err(|e| format!("could not save the password: {e}"))
}

pub fn forget_password(server: &str, extension: &str) {
    if let Some(entry) = keychain_entry(server, extension) {
        let _ = entry.delete_credential();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_from_older_versions_default_to_english() {
        let old = r#"{ "server": "10.0.0.1:5060", "extension": "300", "history": [] }"#;
        let stored: Stored = serde_json::from_str(old).unwrap();
        assert_eq!(stored.language, Lang::English);
        assert_eq!(stored.extension, "300");
    }

    #[test]
    fn language_survives_a_round_trip() {
        let stored = Stored {
            language: Lang::Russian,
            ..Default::default()
        };
        let text = serde_json::to_string(&stored).unwrap();
        let back: Stored = serde_json::from_str(&text).unwrap();
        assert_eq!(back.language, Lang::Russian);
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rsp-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn saving_replaces_the_file_and_leaves_no_draft_behind() {
        let dir = scratch_dir("save");
        let path = dir.join("settings.json");
        let mut stored = Stored {
            extension: "300".into(),
            ..Default::default()
        };
        save_to(&path, &stored);
        stored.extension = "301".into();
        save_to(&path, &stored);
        assert_eq!(load_from(&path).extension, "301");
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_settings_file_is_kept_aside() {
        let dir = scratch_dir("bad");
        let path = dir.join("settings.json");
        std::fs::write(&path, "{ \"server\": \"10.0.0.1\", ").unwrap();
        assert_eq!(load_from(&path).server, "");
        let kept = std::fs::read_to_string(path.with_extension("json.bad")).unwrap();
        assert!(kept.contains("10.0.0.1"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn empty_settings_file_defaults_to_english() {
        let stored: Stored = serde_json::from_str("{}").unwrap();
        assert_eq!(stored.language, Lang::English);
    }
}
