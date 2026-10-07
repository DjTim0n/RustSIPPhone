//! Хранение аккаунта и истории звонков. Пароль лежит в Keychain, остальное — в JSON-файле.

use crate::model::{Account, HistoryEntry};
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
}

fn path() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("RustSIPPhone")
            .join("settings.json"),
    )
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
        .ok_or("Не удалось открыть связку ключей")?
        .set_password(&account.password)
        .map_err(|e| format!("Не удалось сохранить пароль: {e}"))
}

pub fn forget_password(server: &str, extension: &str) {
    if let Some(entry) = keychain_entry(server, extension) {
        let _ = entry.delete_credential();
    }
}
