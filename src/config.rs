use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct AiConfig {
    pub provider: Option<String>,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub skill: Option<String>,
    pub prompt_template: Option<String>,
}

impl AiConfig {
    pub fn provider_key(&self) -> &str {
        self.provider.as_deref().unwrap_or("claude")
    }

    pub fn display_name(&self) -> String {
        match self.provider_key() {
            "codex" => "Codex".to_string(),
            "claude" => "Claude".to_string(),
            other => other.to_string(),
        }
    }

    pub fn command_name(&self) -> String {
        if let Some(command) = &self.command {
            return command.clone();
        }
        match self.provider_key() {
            "codex" => "codex".to_string(),
            _ => "claude".to_string(),
        }
    }

    pub fn skill_name(&self) -> String {
        self.skill
            .clone()
            .unwrap_or_else(|| "code-review".to_string())
    }

}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub repos_root: Option<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub ai: AiConfig,
}

pub fn config_path() -> PathBuf {
    // Use consistent config directory:
    // - macOS/Linux: ~/.config/reviewer
    // - Windows: C:\Users\<User>\AppData\Roaming\reviewer
    #[cfg(target_os = "windows")]
    {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("reviewer")
            .join("config.json")
    }

    #[cfg(not(target_os = "windows"))]
    {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config")
            .join("reviewer")
            .join("config.json")
    }
}

pub fn config_dir() -> PathBuf {
    config_path()
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf()
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
