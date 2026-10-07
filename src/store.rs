//! Storage for the account and call history. The password lives in the keychain, the rest in a JSON file.

use crate::i18n::Lang;
use crate::model::{Account, HistoryEntry};
use crate::settings::{AudioSettings, CallSettings, ConnectionSettings};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(stored: &Stored) {
    let Some(path) = path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string_pretty(stored) {
        let _ = std::fs::write(path, text);
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

    #[test]
    fn empty_settings_file_defaults_to_english() {
        let stored: Stored = serde_json::from_str("{}").unwrap();
        assert_eq!(stored.language, Lang::English);
    }
}
