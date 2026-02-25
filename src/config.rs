use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
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

fn ensure_object(value: &mut Value) -> &mut Map<String, Value> {
    if !value.is_object() {
        *value = Value::Object(Map::new());
    }
    value
        .as_object_mut()
        .expect("value was converted to object")
}

fn merge_known_subobject(
    existing: &mut Map<String, Value>,
    updated: &Map<String, Value>,
    field: &str,
    known_fields: &[&str],
) {
    let Some(updated_subobject) = updated.get(field).and_then(Value::as_object) else {
        return;
    };

    let existing_subobject = ensure_object(
        existing
            .entry(field.to_string())
            .or_insert_with(|| Value::Object(Map::new())),
    );

    for subfield in known_fields {
        if let Some(value) = updated_subobject.get(*subfield) {
            existing_subobject.insert((*subfield).to_string(), value.clone());
        }
    }
}

fn merge_with_existing_config(existing: Value, updated: Value) -> Value {
    let mut existing = existing;
    let mut updated = updated;

    let Some(updated_object) = updated.as_object_mut() else {
        return updated;
    };

    let existing_object = ensure_object(&mut existing);
    for field in ["repos_root", "exclude"] {
        if let Some(value) = updated_object.get(field) {
            existing_object.insert(field.to_string(), value.clone());
        }
    }

    merge_known_subobject(
        existing_object,
        updated_object,
        "ai",
        &[
            "provider",
            "command",
            "args",
            "skill",
            "prompt_template",
            "launcher",
            "terminal_app",
            "terminal_launch_mode",
            "aoe_profile",
            "aoe_group",
        ],
    );

    merge_known_subobject(
        existing_object,
        updated_object,
        "daemon",
        &[
            "poll_interval_sec",
            "exclude_repos",
            "initialized",
            "include_drafts",
            "repo_subpath_filters",
        ],
    );

    existing
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
    let updated = serde_json::to_value(config)?;
    let existing = if path.exists() {
        let contents = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        serde_json::from_str(&contents).with_context(|| {
            format!(
                "Invalid config file {}. Check for typos/unknown fields and JSON syntax.",
                path.display()
            )
        })?
    } else {
        Value::Object(Map::new())
    };

    let merged = merge_with_existing_config(existing, updated);
    let json = serde_json::to_string_pretty(&merged)?;
    std::fs::write(&path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{merge_with_existing_config, parse_config};
    use serde_json::json;

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

    #[test]
    fn merge_with_existing_config_preserves_unknown_daemon_fields() {
        let existing = json!({
          "repos_root": "/tmp/repos",
          "daemon": {
            "poll_interval_sec": 30,
            "exclude_repos": ["org/legacy"],
            "initialized": false,
            "include_drafts": true,
            "repo_subpath_filters": {
              "org/repo": ["services/payments"]
            },
            "future_daemon_field": {
              "enabled": true
            }
          },
          "future_top_level": {
            "value": 123
          }
        });

        let updated = json!({
          "repos_root": "/tmp/repos-new",
          "exclude": [],
          "ai": {
            "provider": null,
            "command": null,
            "args": [],
            "skill": null,
            "prompt_template": null,
            "launcher": null,
            "terminal_app": null,
            "terminal_launch_mode": null,
            "aoe_profile": null,
            "aoe_group": null
          },
          "daemon": {
            "poll_interval_sec": 60,
            "exclude_repos": [],
            "initialized": true,
            "include_drafts": false,
            "repo_subpath_filters": {}
          }
        });

        let merged = merge_with_existing_config(existing, updated);
        assert_eq!(merged["repos_root"], json!("/tmp/repos-new"));
        assert_eq!(merged["daemon"]["initialized"], json!(true));
        assert_eq!(
            merged["daemon"]["future_daemon_field"],
            json!({"enabled": true})
        );
        assert_eq!(merged["future_top_level"], json!({"value": 123}));
    }

    #[test]
    fn merge_with_existing_config_overwrites_known_daemon_fields() {
        let existing = json!({
          "daemon": {
            "poll_interval_sec": 10,
            "exclude_repos": ["org/legacy"],
            "initialized": false,
            "include_drafts": true,
            "repo_subpath_filters": {
              "org/repo": ["a/b"]
            },
            "future_daemon_field": "keep"
          }
        });

        let updated = json!({
          "repos_root": null,
          "exclude": [],
          "ai": {
            "provider": null,
            "command": null,
            "args": [],
            "skill": null,
            "prompt_template": null,
            "launcher": null,
            "terminal_app": null,
            "terminal_launch_mode": null,
            "aoe_profile": null,
            "aoe_group": null
          },
          "daemon": {
            "poll_interval_sec": 60,
            "exclude_repos": [],
            "initialized": true,
            "include_drafts": false,
            "repo_subpath_filters": {}
          }
        });

        let merged = merge_with_existing_config(existing, updated);
        assert_eq!(merged["daemon"]["poll_interval_sec"], json!(60));
        assert_eq!(merged["daemon"]["exclude_repos"], json!([]));
        assert_eq!(merged["daemon"]["repo_subpath_filters"], json!({}));
        assert_eq!(merged["daemon"]["future_daemon_field"], json!("keep"));
    }
}
