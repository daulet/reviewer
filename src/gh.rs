use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Deserialize)]
struct RepoInfo {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

#[derive(Debug, Deserialize)]
pub struct Author {
    pub login: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Review {
    pub author: Option<Author>,
    pub state: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Comment {
    pub author: Option<Author>,
    pub body: String,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct PrData {
    number: u64,
    title: String,
    author: Option<Author>,
    body: Option<String>,
    url: String,
    #[serde(rename = "updatedAt")]
    updated_at: DateTime<Utc>,
    additions: Option<u64>,
    deletions: Option<u64>,
    reviews: Option<Vec<Review>>,
    #[serde(rename = "isDraft")]
    is_draft: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub body: String,
    pub repo_path: PathBuf,
    pub repo_name: String,
    pub url: String,
    pub updated_at: DateTime<Utc>,
    pub additions: u64,
    pub deletions: u64,
    pub is_draft: bool,
}

pub fn get_current_user() -> Result<String> {
    let output = Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .output()
        .context("Failed to run gh cli")?;

    if !output.status.success() {
        anyhow::bail!("gh auth failed - is gh cli authenticated?");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn get_repo_info(repo_path: &PathBuf) -> Option<RepoInfo> {
    let output = Command::new("gh")
        .args(["repo", "view", "--json", "nameWithOwner"])
        .current_dir(repo_path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    serde_json::from_slice(&output.stdout).ok()
}

fn get_open_prs(repo_path: &PathBuf) -> Vec<PrData> {
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--json",
            "number,title,author,body,url,updatedAt,additions,deletions,reviews,isDraft",
            "--limit",
            "100",
        ])
        .current_dir(repo_path)
        .output()
        .ok();

    match output {
        Some(o) if o.status.success() => serde_json::from_slice(&o.stdout).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn has_user_approved(pr: &PrData, username: &str) -> bool {
    pr.reviews
        .as_ref()
        .map(|reviews| {
            reviews.iter().any(|r| {
                r.author
                    .as_ref()
                    .and_then(|a| a.login.as_ref())
                    .map(|login| login == username)
                    .unwrap_or(false)
                    && r.state.as_deref() == Some("APPROVED")
            })
        })
        .unwrap_or(false)
}

pub fn fetch_prs_for_repo(repo_path: &PathBuf, username: &str, include_drafts: bool) -> Vec<PullRequest> {
    let repo_info = match get_repo_info(repo_path) {
        Some(info) => info,
        None => return Vec::new(),
    };

    let prs_data = get_open_prs(repo_path);
    let mut prs = Vec::new();

    for pr_data in prs_data {
        // Skip drafts unless include_drafts is true
        if !include_drafts && pr_data.is_draft.unwrap_or(false) {
            continue;
        }

        if has_user_approved(&pr_data, username) {
            continue;
        }

        let pr_author = pr_data
            .author
            .as_ref()
            .and_then(|a| a.login.as_ref())
            .map(|s| s.as_str())
            .unwrap_or("unknown");

        if pr_author == username {
            continue;
        }

        prs.push(PullRequest {
            number: pr_data.number,
            title: pr_data.title,
            author: pr_author.to_string(),
            body: pr_data.body.unwrap_or_default(),
            repo_path: repo_path.clone(),
            repo_name: repo_info.name_with_owner.clone(),
            url: pr_data.url,
            updated_at: pr_data.updated_at,
            additions: pr_data.additions.unwrap_or(0),
            deletions: pr_data.deletions.unwrap_or(0),
            is_draft: pr_data.is_draft.unwrap_or(false),
        });
    }

    prs
}

pub fn get_pr_diff(pr: &PullRequest) -> Result<String> {
    let output = Command::new("gh")
        .args(["pr", "diff", &pr.number.to_string()])
        .current_dir(&pr.repo_path)
        .output()
        .context("Failed to get PR diff")?;

    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.contains("too_large") {
        anyhow::bail!("Failed to get diff: {}", stderr);
    }

    // Fallback: fetch diff locally for large PRs
    get_pr_diff_local(pr)
}

#[derive(Debug, Deserialize)]
struct PrRefs {
    #[serde(rename = "baseRefOid")]
    base_ref_oid: String,
    #[serde(rename = "headRefOid")]
    head_ref_oid: String,
}

fn get_pr_diff_local(pr: &PullRequest) -> Result<String> {
    // Get the base and head commit SHAs
    let output = Command::new("gh")
        .args([
            "pr", "view",
            &pr.number.to_string(),
            "--json", "baseRefOid,headRefOid",
        ])
        .current_dir(&pr.repo_path)
        .output()
        .context("Failed to get PR refs")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to get PR refs: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let refs: PrRefs = serde_json::from_slice(&output.stdout)
        .context("Failed to parse PR refs")?;

    // Fetch the head commit
    let fetch_output = Command::new("git")
        .args(["fetch", "origin", &refs.head_ref_oid])
        .current_dir(&pr.repo_path)
        .output()
        .context("Failed to fetch head ref")?;

    if !fetch_output.status.success() {
        // Try fetching via PR ref instead
        let pr_ref = format!("refs/pull/{}/head", pr.number);
        let _ = Command::new("git")
            .args(["fetch", "origin", &pr_ref])
            .current_dir(&pr.repo_path)
            .output();
    }

    // Generate diff locally
    let diff_output = Command::new("git")
        .args(["diff", &format!("{}...{}", refs.base_ref_oid, refs.head_ref_oid)])
        .current_dir(&pr.repo_path)
        .output()
        .context("Failed to generate local diff")?;

    if !diff_output.status.success() {
        anyhow::bail!(
            "Failed to generate diff: {}",
            String::from_utf8_lossy(&diff_output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&diff_output.stdout).to_string())
}

pub fn get_pr_comments(pr: &PullRequest) -> Result<Vec<Comment>> {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            &pr.number.to_string(),
            "--json",
            "comments",
            "--jq",
            ".comments",
        ])
        .current_dir(&pr.repo_path)
        .output()
        .context("Failed to get PR comments")?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let comments: Vec<Comment> = serde_json::from_slice(&output.stdout).unwrap_or_default();
    Ok(comments)
}

pub fn add_pr_comment(pr: &PullRequest, comment: &str) -> Result<()> {
    let output = Command::new("gh")
        .args(["pr", "comment", &pr.number.to_string(), "--body", comment])
        .current_dir(&pr.repo_path)
        .output()
        .context("Failed to add comment")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to add comment: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

pub fn approve_pr(pr: &PullRequest, comment: Option<&str>) -> Result<()> {
    let pr_number = pr.number.to_string();
    let mut args = vec!["pr", "review", &pr_number, "--approve"];

    let body_arg;
    if let Some(c) = comment {
        body_arg = c.to_string();
        args.push("--body");
        args.push(&body_arg);
    }

    let output = Command::new("gh")
        .args(&args)
        .current_dir(&pr.repo_path)
        .output()
        .context("Failed to approve PR")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to approve PR: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Create a worktree for a PR and return the path
pub fn create_pr_worktree(pr: &PullRequest, repos_root: &std::path::Path) -> Result<std::path::PathBuf> {
    let worktree_base = repos_root.join(".worktrees");
    std::fs::create_dir_all(&worktree_base)?;

    let worktree_name = format!("{}-pr-{}", pr.repo_name.replace('/', "-"), pr.number);
    let worktree_path = worktree_base.join(&worktree_name);

    // Remove existing worktree if it exists
    if worktree_path.exists() {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force", worktree_path.to_str().unwrap()])
            .current_dir(&pr.repo_path)
            .output();
        // Also try removing the directory directly if worktree remove failed
        let _ = std::fs::remove_dir_all(&worktree_path);
    }

    // Fetch the PR head ref
    let pr_ref = format!("refs/pull/{}/head", pr.number);
    let fetch_output = Command::new("git")
        .args(["fetch", "origin", &pr_ref])
        .current_dir(&pr.repo_path)
        .output()
        .context("Failed to fetch PR ref")?;

    if !fetch_output.status.success() {
        anyhow::bail!(
            "Failed to fetch PR: {}",
            String::from_utf8_lossy(&fetch_output.stderr)
        );
    }

    // Create worktree at FETCH_HEAD
    let output = Command::new("git")
        .args([
            "worktree",
            "add",
            worktree_path.to_str().unwrap(),
            "FETCH_HEAD",
        ])
        .current_dir(&pr.repo_path)
        .output()
        .context("Failed to create worktree")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to create worktree: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(worktree_path)
}

/// Launch Claude Code CLI in a directory with code review prompt
pub fn launch_claude(working_dir: &std::path::Path, pr: &PullRequest) -> Result<()> {
    // Prompt that triggers the code-review skill with PR context
    let prompt = format!(
        "Review PR #{} in repo {}. Title: \"{}\". \
         Use the code-review skill to analyze changes, present each issue for approval, \
         and submit approved comments using gh CLI. Follow guidelines in ~/.config/reviewer/review_guide.md",
        pr.number, pr.repo_name, pr.title.replace('"', "\\\"")
    );

    #[cfg(target_os = "macos")]
    {
        let escaped_prompt = prompt.replace('\'', "'\\''").replace('"', "\\\"");
        let script = format!(
            r#"tell application "Terminal"
                activate
                do script "cd '{}' && claude '{}'"
            end tell"#,
            working_dir.display(),
            escaped_prompt
        );
        Command::new("osascript")
            .args(["-e", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("Failed to launch Terminal")?;
    }

    #[cfg(target_os = "linux")]
    {
        let escaped_prompt = prompt.replace('\'', "'\\''");
        let terminals = ["gnome-terminal", "konsole", "xterm"];
        let mut launched = false;
        for term in terminals {
            let result = match term {
                "gnome-terminal" => Command::new(term)
                    .args(["--", "bash", "-c", &format!("cd '{}' && claude '{}'; exec bash", working_dir.display(), escaped_prompt)])
                    .spawn(),
                "konsole" => Command::new(term)
                    .args(["-e", "bash", "-c", &format!("cd '{}' && claude '{}'; exec bash", working_dir.display(), escaped_prompt)])
                    .spawn(),
                _ => Command::new(term)
                    .args(["-e", &format!("cd '{}' && claude '{}'", working_dir.display(), escaped_prompt)])
                    .spawn(),
            };
            if result.is_ok() {
                launched = true;
                break;
            }
        }
        if !launched {
            anyhow::bail!("Could not find a terminal emulator");
        }
    }

    Ok(())
}
