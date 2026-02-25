use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

fn default_poll_interval_sec() -> u64 {
    60
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct AiConfig {
    pub provider: Option<String>,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub skill: Option<String>,
    pub prompt_template: Option<String>,
    pub launcher: Option<String>,
    pub terminal_app: Option<String>,
    pub terminal_launch_mode: Option<String>,
    pub aoe_profile: Option<String>,
    pub aoe_group: Option<String>,
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

    pub fn launcher_key(&self) -> String {
        self.launcher
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("terminal")
            .to_ascii_lowercase()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    #[serde(default = "default_poll_interval_sec")]
    pub poll_interval_sec: u64,
    #[serde(default)]
    pub exclude_repos: Vec<String>,
    #[serde(default)]
    pub initialized: bool,
    #[serde(default)]
    pub include_drafts: bool,
    #[serde(default)]
    pub repo_subpath_filters: HashMap<String, Vec<String>>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            poll_interval_sec: default_poll_interval_sec(),
            exclude_repos: Vec::new(),
            initialized: false,
            include_drafts: false,
            repo_subpath_filters: HashMap::new(),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub repos_root: Option<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub ai: AiConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
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

fn parse_config(contents: &str) -> Result<Config> {
    serde_json::from_str(contents).context("Invalid reviewer config JSON")
}

pub fn load_config() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        return Ok(Config::default());
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;
    parse_config(&contents).with_context(|| {
        format!(
            "Invalid config file {}. Check for typos/unknown fields and JSON syntax.",
            path.display()
        )
    })
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

#[cfg(test)]
mod tests {
    use super::parse_config;

    #[test]
    fn parse_config_rejects_unknown_top_level_field() {
        let json = r#"
        {
          "repos_root": "/tmp/repos",
          "typo_field": true
        }
        "#;

        let err = parse_config(json).expect_err("expected parse error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unknown field `typo_field`"),
            "unexpected error: {}",
            msg
        );
    }

    #[test]
    fn parse_config_rejects_unknown_nested_field() {
        let json = r#"
        {
          "ai": {
            "provider": "codex",
            "launchr": "aoe"
          }
        }
        "#;

        let err = parse_config(json).expect_err("expected parse error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unknown field `launchr`"),
            "unexpected error: {}",
            msg
        );
    }
}
