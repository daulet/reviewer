use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub repos_root: Option<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

pub fn config_path() -> PathBuf {
    // Use platform-appropriate config directory:
    // - Windows: C:\Users\<User>\AppData\Roaming\reviewer
    // - macOS: ~/Library/Application Support/reviewer
    // - Linux: ~/.config/reviewer
    dirs::config_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        })
        .join("reviewer")
        .join("config.json")
}

pub fn load_config() -> Config {
    let path = config_path();
    if !path.exists() {
        return Config::default();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_config(config: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(config)?;
    std::fs::write(&path, json)?;
    Ok(())
}
