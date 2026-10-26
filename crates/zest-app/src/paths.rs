//! Folders and files Zest uses.

use std::path::PathBuf;

fn env_dir(name: &str) -> PathBuf {
    std::env::var_os(name).map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// `%APPDATA%\Zest`
pub fn config_dir() -> PathBuf {
    env_dir("APPDATA").join("Zest")
}

/// `%APPDATA%\Zest\config.toml`
pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// `%LOCALAPPDATA%\Zest`
pub fn local_dir() -> PathBuf {
    env_dir("LOCALAPPDATA").join("Zest")
}

/// `%LOCALAPPDATA%\Zest\logs`
pub fn logs_dir() -> PathBuf {
    local_dir().join("logs")
}

/// `%LOCALAPPDATA%\Zest\state.json`
pub fn state_path() -> PathBuf {
    local_dir().join("state.json")
}

/// `%LOCALAPPDATA%\Zest\history.jsonl`
pub fn history_path() -> PathBuf {
    local_dir().join("history.jsonl")
}
