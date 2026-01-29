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
            "number,title,author,body,url,updatedAt,additions,deletions,reviews",
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

pub fn fetch_prs_for_repo(repo_path: &PathBuf, username: &str) -> Vec<PullRequest> {
    let repo_info = match get_repo_info(repo_path) {
        Some(info) => info,
        None => return Vec::new(),
    };

    let prs_data = get_open_prs(repo_path);
    let mut prs = Vec::new();

    for pr_data in prs_data {
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

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("too_large") {
            return Ok("Diff too large to display".to_string());
        }
        anyhow::bail!("Failed to get diff: {}", stderr);
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
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
