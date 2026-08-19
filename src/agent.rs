use crate::gh::PullRequest;
use anyhow::{Context, Result};
use std::process::Command;

const CAPTURE_START_LINE: &str = "-200";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPane {
    pub target: String,
    pub session_name: String,
    pub window_index: String,
    pub window_name: String,
    pub pane_id: String,
    pub pane_index: String,
    pub pane_command: String,
    pub pane_title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPreview {
    pub expected_slug: String,
    pub pane: Option<AgentPane>,
    pub output: String,
    pub error: Option<String>,
}

pub fn pr_agent_slug(pr: &PullRequest) -> String {
    format!("{}-pr-{}", pr.repo_name.replace('/', "-"), pr.number)
}

fn pr_agent_search_terms(pr: &PullRequest) -> Vec<String> {
    vec![
        pr_agent_slug(pr),
        format!("{}#{}", pr.repo_name, pr.number),
        format!("{}/pull/{}", pr.repo_name, pr.number),
    ]
}

fn parse_pane_line(line: &str) -> Option<AgentPane> {
    let mut parts = line.splitn(7, '\t');
    let session_name = parts.next()?.to_string();
    let window_index = parts.next()?.to_string();
    let window_name = parts.next()?.to_string();
    let pane_id = parts.next()?.to_string();
    let pane_index = parts.next()?.to_string();
    let pane_command = parts.next()?.to_string();
    let pane_title = parts.next()?.to_string();
    let target = if pane_id.starts_with('%') {
        pane_id.clone()
    } else {
        format!("{session_name}:{window_index}.{pane_id}")
    };

    Some(AgentPane {
        target,
        session_name,
        window_index,
        window_name,
        pane_id,
        pane_index,
        pane_command,
        pane_title,
    })
}

fn pane_score(pane: &AgentPane, terms: &[String]) -> Option<u8> {
    for term in terms {
        if pane.pane_title == *term {
            return Some(0);
        }
        if pane.window_name == *term {
            return Some(1);
        }
    }

    for term in terms {
        if pane.pane_title.contains(term) {
            return Some(2);
        }
        if pane.window_name.contains(term) {
            return Some(3);
        }
        if pane.session_name.contains(term) {
            return Some(4);
        }
    }

    None
}

pub fn list_agent_panes() -> Result<Vec<AgentPane>> {
    let output = Command::new("tmux")
        .args([
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{window_index}\t#{window_name}\t#{pane_id}\t#{pane_index}\t#{pane_current_command}\t#{pane_title}",
        ])
        .output()
        .context("Failed to list tmux panes")?;

    if !output.status.success() {
        anyhow::bail!(
            "tmux list-panes failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_pane_line)
        .collect())
}

pub fn find_agent_pane(pr: &PullRequest) -> Result<Option<AgentPane>> {
    let terms = pr_agent_search_terms(pr);
    let mut matches = list_agent_panes()?
        .into_iter()
        .filter_map(|pane| pane_score(&pane, &terms).map(|score| (score, pane)))
        .collect::<Vec<_>>();

    matches.sort_by_key(|(score, _)| *score);
    Ok(matches.into_iter().next().map(|(_, pane)| pane))
}

pub fn capture_agent_pane(target: &str) -> Result<String> {
    let output = Command::new("tmux")
        .args(["capture-pane", "-p", "-t", target, "-S", CAPTURE_START_LINE])
        .output()
        .context("Failed to capture tmux pane")?;

    if !output.status.success() {
        anyhow::bail!(
            "tmux capture-pane failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn preview_agent_session(pr: &PullRequest) -> AgentPreview {
    let expected_slug = pr_agent_slug(pr);
    match find_agent_pane(pr) {
        Ok(Some(pane)) => match capture_agent_pane(&pane.target) {
            Ok(output) => AgentPreview {
                expected_slug,
                pane: Some(pane),
                output,
                error: None,
            },
            Err(err) => AgentPreview {
                expected_slug,
                pane: Some(pane),
                output: String::new(),
                error: Some(format!("{:#}", err)),
            },
        },
        Ok(None) => AgentPreview {
            expected_slug,
            pane: None,
            output: String::new(),
            error: None,
        },
        Err(err) => AgentPreview {
            expected_slug,
            pane: None,
            output: String::new(),
            error: Some(format!("{:#}", err)),
        },
    }
}

fn tmux_display_target(target: &str, format: &str) -> Result<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-p", "-t", target, format])
        .output()
        .context("Failed to resolve tmux target")?;

    if !output.status.success() {
        anyhow::bail!(
            "tmux display-message failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn tmux_select_target(target: &str) -> Result<()> {
    let status = Command::new("tmux")
        .args(["select-pane", "-t", target])
        .status()
        .context("Failed to select tmux pane")?;

    if !status.success() {
        anyhow::bail!("tmux select-pane failed with {}", status);
    }

    Ok(())
}

pub fn switch_or_attach(target: &str) -> Result<()> {
    let session_target = tmux_display_target(target, "#{session_name}")?;
    let window_target = tmux_display_target(target, "#{session_name}:#{window_index}")?;
    tmux_select_target(target)?;

    let args = if std::env::var_os("TMUX").is_some() {
        vec!["switch-client", "-t", window_target.as_str()]
    } else {
        vec!["attach-session", "-t", session_target.as_str()]
    };

    let status = Command::new("tmux")
        .args(args)
        .status()
        .context("Failed to attach to tmux session")?;

    if !status.success() {
        anyhow::bail!("tmux attach failed with {}", status);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{pane_score, parse_pane_line, pr_agent_slug};
    use crate::gh::{PullRequest, ReviewState};
    use chrono::Utc;
    use std::path::PathBuf;

    fn make_test_pr() -> PullRequest {
        PullRequest {
            number: 199,
            title: "Test PR".to_string(),
            author: "alice".to_string(),
            author_kind: Some("User".to_string()),
            body: String::new(),
            repo_path: PathBuf::from("/tmp/repo"),
            repo_name: "nvidia-lpu/cyborg".to_string(),
            url: "https://github.com/nvidia-lpu/cyborg/pull/199".to_string(),
            updated_at: Utc::now(),
            additions: 1,
            deletions: 1,
            is_draft: false,
            review_state: ReviewState::Pending,
            details_loaded: true,
        }
    }

    #[test]
    fn pr_agent_slug_matches_maestro_pane_title() {
        assert_eq!(pr_agent_slug(&make_test_pr()), "nvidia-lpu-cyborg-pr-199");
    }

    #[test]
    fn parse_pane_line_uses_pane_id_target() {
        let pane = parse_pane_line("maestro\t0\tcodex\t%358\t0\tcodex\tnvidia-lpu-cyborg-pr-199")
            .expect("pane should parse");

        assert_eq!(pane.target, "%358");
        assert_eq!(pane.pane_index, "0");
        assert_eq!(pane.pane_title, "nvidia-lpu-cyborg-pr-199");
    }

    #[test]
    fn pane_score_prefers_exact_pane_title_match() {
        let pane = parse_pane_line("maestro\t0\tcodex\t%358\t0\tcodex\tnvidia-lpu-cyborg-pr-199")
            .expect("pane should parse");

        assert_eq!(
            pane_score(&pane, &["nvidia-lpu-cyborg-pr-199".to_string()]),
            Some(0)
        );
    }
}
