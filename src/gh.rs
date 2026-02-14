use crate::config::{self, AiConfig};
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

#[derive(Debug, Clone, Deserialize)]
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

/// A review comment on a specific line in the diff
#[derive(Debug, Clone, Deserialize)]
pub struct ReviewComment {
    pub user: Option<Author>,
    pub body: String,
    pub path: String,
    pub line: Option<u32>,
    #[allow(dead_code)] // Reserved for future diff context handling
    #[serde(rename = "original_line")]
    pub original_line: Option<u32>,
    #[serde(rename = "diff_hunk")]
    pub diff_hunk: String,
    #[serde(rename = "created_at")]
    pub created_at: DateTime<Utc>,
    #[serde(rename = "in_reply_to_id")]
    pub in_reply_to_id: Option<u64>,
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
    #[serde(rename = "reviewDecision")]
    review_decision: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchRepository {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

/// PR data from gh search prs command (limited fields available)
#[derive(Debug, Deserialize)]
struct SearchPrData {
    number: u64,
    title: String,
    author: Option<Author>,
    body: Option<String>,
    url: String,
    #[serde(rename = "updatedAt")]
    updated_at: DateTime<Utc>,
    #[serde(rename = "isDraft")]
    is_draft: Option<bool>,
    repository: SearchRepository,
}

/// Review state for user's PRs
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReviewState {
    /// Approved and ready to merge
    Approved,
    /// Changes requested - needs attention
    ChangesRequested,
    /// Pending review - no action yet
    Pending,
    /// Draft PR
    Draft,
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
    pub review_state: ReviewState,
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
            "number,title,author,body,url,updatedAt,additions,deletions,reviews,isDraft,reviewDecision",
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

fn determine_review_state(pr_data: &PrData) -> ReviewState {
    if pr_data.is_draft.unwrap_or(false) {
        return ReviewState::Draft;
    }
    match pr_data.review_decision.as_deref() {
        Some("APPROVED") => ReviewState::Approved,
        Some("CHANGES_REQUESTED") => ReviewState::ChangesRequested,
        _ => ReviewState::Pending,
    }
}

pub fn fetch_prs_for_repo(
    repo_path: &PathBuf,
    username: &str,
    include_drafts: bool,
) -> Vec<PullRequest> {
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

        let review_state = determine_review_state(&pr_data);

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
            review_state,
        });
    }

    prs
}

/// Search for all PRs authored by the current user across all repos
pub fn search_my_prs(include_drafts: bool) -> Vec<PullRequest> {
    use rayon::prelude::*;

    let output = Command::new("gh")
        .args([
            "search",
            "prs",
            "--state=open",
            "--author=@me",
            "--json",
            "number,title,author,body,url,updatedAt,isDraft,repository",
            "--limit",
            "100",
        ])
        .output()
        .ok();

    let prs_data: Vec<SearchPrData> = match output {
        Some(o) if o.status.success() => serde_json::from_slice(&o.stdout).unwrap_or_default(),
        _ => return Vec::new(),
    };

    // Filter drafts first, then fetch review states in parallel
    let filtered: Vec<_> = prs_data
        .into_iter()
        .filter(|pr| include_drafts || !pr.is_draft.unwrap_or(false))
        .collect();

    let mut prs: Vec<PullRequest> = filtered
        .par_iter()
        .map(|pr_data| {
            let pr_author = pr_data
                .author
                .as_ref()
                .and_then(|a| a.login.as_ref())
                .map(|s| s.as_str())
                .unwrap_or("unknown");

            // Search API doesn't provide reviewDecision, fetch in parallel
            let review_state = if pr_data.is_draft.unwrap_or(false) {
                ReviewState::Draft
            } else {
                fetch_pr_review_state(&pr_data.repository.name_with_owner, pr_data.number)
            };

            PullRequest {
                number: pr_data.number,
                title: pr_data.title.clone(),
                author: pr_author.to_string(),
                body: pr_data.body.clone().unwrap_or_default(),
                repo_path: PathBuf::new(),
                repo_name: pr_data.repository.name_with_owner.clone(),
                url: pr_data.url.clone(),
                updated_at: pr_data.updated_at,
                additions: 0,
                deletions: 0,
                is_draft: pr_data.is_draft.unwrap_or(false),
                review_state,
            }
        })
        .collect();

    // Sort by review state priority, then by most recent
    prs.sort_by(|a, b| match a.review_state.cmp(&b.review_state) {
        std::cmp::Ordering::Equal => b.updated_at.cmp(&a.updated_at),
        other => other,
    });

    prs
}

/// Fetch the review decision for a specific PR
fn fetch_pr_review_state(repo: &str, pr_number: u64) -> ReviewState {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--repo",
            repo,
            "--json",
            "reviewDecision",
        ])
        .output()
        .ok();

    #[derive(Deserialize)]
    struct ReviewDecisionResponse {
        #[serde(rename = "reviewDecision")]
        review_decision: Option<String>,
    }

    match output {
        Some(o) if o.status.success() => {
            let response: Option<ReviewDecisionResponse> = serde_json::from_slice(&o.stdout).ok();
            match response.and_then(|r| r.review_decision) {
                Some(ref s) if s == "APPROVED" => ReviewState::Approved,
                Some(ref s) if s == "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
                _ => ReviewState::Pending,
            }
        }
        _ => ReviewState::Pending,
    }
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
            "pr",
            "view",
            &pr.number.to_string(),
            "--json",
            "baseRefOid,headRefOid",
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

    let refs: PrRefs = serde_json::from_slice(&output.stdout).context("Failed to parse PR refs")?;

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
        .args([
            "diff",
            &format!("{}...{}", refs.base_ref_oid, refs.head_ref_oid),
        ])
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

/// Get review comments (line-level comments on the diff) for a PR
pub fn get_review_comments(pr: &PullRequest) -> Result<Vec<ReviewComment>> {
    let api_path = format!("repos/{}/pulls/{}/comments", pr.repo_name, pr.number);
    let output = Command::new("gh")
        .args(["api", &api_path])
        .output()
        .context("Failed to get review comments")?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let comments: Vec<ReviewComment> = serde_json::from_slice(&output.stdout).unwrap_or_default();
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

/// Add a line-level comment to a PR using the reviews API
/// `side` should be "LEFT" for removed lines (old file) or "RIGHT" for added/context lines (new file)
pub fn add_line_comment(
    pr: &PullRequest,
    file_path: &str,
    line: u32,
    side: &str,
    comment: &str,
) -> Result<()> {
    // Use the reviews endpoint with a comments array
    let api_path = format!("repos/{}/pulls/{}/reviews", pr.repo_name, pr.number);

    // Build complete JSON payload
    let payload = serde_json::json!({
        "event": "COMMENT",
        "body": "",
        "comments": [{
            "path": file_path,
            "line": line,
            "side": side,
            "body": comment
        }]
    });

    let mut child = Command::new("gh")
        .args(["api", &api_path, "-X", "POST", "--input", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .current_dir(&pr.repo_path)
        .spawn()
        .context("Failed to spawn gh command")?;

    // Write JSON payload to stdin
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        stdin
            .write_all(payload.to_string().as_bytes())
            .context("Failed to write to gh stdin")?;
    }

    let output = child.wait_with_output().context("Failed to wait for gh")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("Line comment API failed: {}", stderr);
        eprintln!("Payload was: {}", payload);
        // If line comment fails, fall back to a general comment with file:line reference
        let fallback_comment = format!("**{}:{}**\n\n{}", file_path, line, comment);
        return add_pr_comment(pr, &fallback_comment).context(format!(
            "Line comment failed ({}), fallback also failed",
            stderr
        ));
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

/// Close a PR with an optional comment
pub fn close_pr(pr: &PullRequest, comment: Option<&str>) -> Result<()> {
    // Add comment first if provided (closing comment)
    if let Some(c) = comment {
        add_pr_comment(pr, c)?;
    }

    let output = Command::new("gh")
        .args([
            "pr",
            "close",
            &pr.number.to_string(),
            "--repo",
            &pr.repo_name,
        ])
        .current_dir(&pr.repo_path)
        .output()
        .context("Failed to close PR")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to close PR: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Open PR in web browser
pub fn open_pr_in_browser(pr: &PullRequest) -> Result<()> {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            &pr.number.to_string(),
            "--repo",
            &pr.repo_name,
            "--web",
        ])
        .output()
        .context("Failed to open PR in browser")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to open PR: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// CI check status
#[derive(Debug, Clone)]
pub struct CheckStatus {
    #[allow(dead_code)] // Kept for potential future detailed CI view
    pub name: String,
    pub status: CheckState,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CheckState {
    Pending,
    Success,
    Failure,
    Neutral,
}

/// Get CI/status checks for a PR
pub fn get_pr_checks(pr: &PullRequest) -> Result<Vec<CheckStatus>> {
    let output = Command::new("gh")
        .args([
            "pr",
            "checks",
            &pr.number.to_string(),
            "--repo",
            &pr.repo_name,
            "--json",
            "name,state",
        ])
        .output()
        .context("Failed to get PR checks")?;

    if !output.status.success() {
        // No checks might just mean none configured
        return Ok(Vec::new());
    }

    #[derive(Deserialize)]
    struct CheckData {
        name: String,
        state: Option<String>,
    }

    let checks: Vec<CheckData> = serde_json::from_slice(&output.stdout).unwrap_or_default();

    Ok(checks
        .into_iter()
        .map(|c| {
            let status = match c.state.as_deref() {
                Some("SUCCESS") => CheckState::Success,
                Some("FAILURE") | Some("ERROR") => CheckState::Failure,
                Some("NEUTRAL") | Some("SKIPPED") => CheckState::Neutral,
                _ => CheckState::Pending,
            };
            CheckStatus {
                name: c.name,
                status,
            }
        })
        .collect())
}

/// Result of checking if a PR can be merged
#[derive(Debug)]
pub struct MergeStatus {
    pub can_merge: bool,
    pub reason: Option<String>,
}

/// Check if a PR can be merged (no unresolved threads, mergeable state)
pub fn check_merge_status(pr: &PullRequest) -> MergeStatus {
    // Use GraphQL to check reviewThreads for unresolved comments
    let query = format!(
        r#"query {{
            repository(owner: "{}", name: "{}") {{
                pullRequest(number: {}) {{
                    mergeable
                    reviewThreads(first: 100) {{
                        nodes {{
                            isResolved
                        }}
                    }}
                }}
            }}
        }}"#,
        pr.repo_name.split('/').next().unwrap_or(""),
        pr.repo_name.split('/').nth(1).unwrap_or(""),
        pr.number
    );

    let output = Command::new("gh")
        .args(["api", "graphql", "-f", &format!("query={}", query)])
        .output()
        .ok();

    #[derive(Deserialize)]
    struct ReviewThread {
        #[serde(rename = "isResolved")]
        is_resolved: bool,
    }

    #[derive(Deserialize)]
    struct ReviewThreadsNodes {
        nodes: Vec<ReviewThread>,
    }

    #[derive(Deserialize)]
    struct PrInfo {
        mergeable: Option<String>,
        #[serde(rename = "reviewThreads")]
        review_threads: Option<ReviewThreadsNodes>,
    }

    #[derive(Deserialize)]
    struct RepoData {
        #[serde(rename = "pullRequest")]
        pull_request: Option<PrInfo>,
    }

    #[derive(Deserialize)]
    struct GraphQLResponse {
        data: Option<RepositoryWrapper>,
    }

    #[derive(Deserialize)]
    struct RepositoryWrapper {
        repository: Option<RepoData>,
    }

    let response: Option<GraphQLResponse> = output
        .filter(|o| o.status.success())
        .and_then(|o| serde_json::from_slice(&o.stdout).ok());

    let pr_info = response
        .and_then(|r| r.data)
        .and_then(|d| d.repository)
        .and_then(|r| r.pull_request);

    match pr_info {
        Some(info) => {
            // Check for unresolved threads
            if let Some(threads) = info.review_threads {
                let unresolved_count = threads.nodes.iter().filter(|t| !t.is_resolved).count();
                if unresolved_count > 0 {
                    return MergeStatus {
                        can_merge: false,
                        reason: Some(format!("{} unresolved review thread(s)", unresolved_count)),
                    };
                }
            }

            // Check mergeable state
            match info.mergeable.as_deref() {
                Some("MERGEABLE") => MergeStatus {
                    can_merge: true,
                    reason: None,
                },
                Some("CONFLICTING") => MergeStatus {
                    can_merge: false,
                    reason: Some("PR has merge conflicts".to_string()),
                },
                Some("UNKNOWN") => MergeStatus {
                    can_merge: false,
                    reason: Some("Merge status unknown, try again".to_string()),
                },
                _ => MergeStatus {
                    can_merge: false,
                    reason: Some("PR is not mergeable".to_string()),
                },
            }
        }
        None => MergeStatus {
            can_merge: false,
            reason: Some("Failed to check merge status".to_string()),
        },
    }
}

/// Merge a PR using squash merge (preferred), falling back to regular merge
pub fn merge_pr(pr: &PullRequest, delete_branch: bool) -> Result<String> {
    let pr_number = pr.number.to_string();

    // Try squash merge first
    let mut args = vec![
        "pr",
        "merge",
        &pr_number,
        "--repo",
        &pr.repo_name,
        "--squash",
    ];

    if delete_branch {
        args.push("--delete-branch");
    }

    let output = Command::new("gh")
        .args(&args)
        .output()
        .context("Failed to merge PR")?;

    if output.status.success() {
        return Ok("squash".to_string());
    }

    // If squash failed, try regular merge
    let mut args = vec![
        "pr",
        "merge",
        &pr_number,
        "--repo",
        &pr.repo_name,
        "--merge",
    ];

    if delete_branch {
        args.push("--delete-branch");
    }

    let output = Command::new("gh")
        .args(&args)
        .output()
        .context("Failed to merge PR")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to merge PR: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok("merge".to_string())
}

/// Create a worktree for a PR and return the path
pub fn create_pr_worktree(
    pr: &PullRequest,
    repos_root: &std::path::Path,
) -> Result<std::path::PathBuf> {
    let worktree_base = repos_root.join(".worktrees");
    std::fs::create_dir_all(&worktree_base)?;

    let worktree_name = format!("{}-pr-{}", pr.repo_name.replace('/', "-"), pr.number);
    let worktree_path = worktree_base.join(&worktree_name);

    // Remove existing worktree if it exists
    if worktree_path.exists() {
        let _ = Command::new("git")
            .args([
                "worktree",
                "remove",
                "--force",
                worktree_path.to_str().unwrap(),
            ])
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn unix_shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "windows")]
fn windows_cmd_escape(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn build_unix_command(command: &str, args: &[String], prompt: &str) -> String {
    let mut parts = Vec::with_capacity(args.len() + 2);
    parts.push(unix_shell_escape(command));
    for arg in args {
        parts.push(unix_shell_escape(arg));
    }
    parts.push(unix_shell_escape(prompt));
    parts.join(" ")
}

#[cfg(target_os = "windows")]
fn build_windows_command(command: &str, args: &[String], prompt: &str) -> String {
    let mut parts = Vec::with_capacity(args.len() + 2);
    parts.push(windows_cmd_escape(command));
    for arg in args {
        parts.push(windows_cmd_escape(arg));
    }
    parts.push(windows_cmd_escape(prompt));
    parts.join(" ")
}

#[cfg(target_os = "macos")]
fn launch_macos_terminal_applescript(app: &str, command_line: &str) -> Result<()> {
    let escaped_command = command_line.replace('"', "\\\"");
    let script = format!(
        r#"tell application "{app}"
            activate
            do script "{command}"
        end tell"#,
        app = app,
        command = escaped_command
    );
    Command::new("osascript")
        .args(["-e", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to launch {}", app))?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn launch_macos_terminal_open(app: &str, command_line: &str) -> Result<()> {
    Command::new("open")
        .args(["-na", app, "--args", "-e", "bash", "-lc", command_line])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to launch {}", app))?;
    Ok(())
}

fn render_prompt(
    template: &str,
    pr: &PullRequest,
    review_guide: &std::path::Path,
    skill_invocation: &str,
) -> String {
    template
        .replace("{pr_number}", &pr.number.to_string())
        .replace("{repo}", &pr.repo_name)
        .replace("{title}", &pr.title)
        .replace("{review_guide}", &review_guide.display().to_string())
        .replace("{skill}", skill_invocation)
}

/// Launch a code review assistant CLI in a directory with a review prompt
pub fn launch_ai(working_dir: &std::path::Path, pr: &PullRequest, ai: &AiConfig) -> Result<()> {
    let provider = ai.provider_key();
    let command = ai.command_name();

    // Get platform-appropriate config directory for review guide reference
    let config_dir = config::config_dir();
    let review_guide = config_dir.join("review_guide.md");

    let skill_name = ai.skill_name();
    let skill_invocation = if provider == "codex" {
        format!("${}", skill_name)
    } else {
        format!("{} skill", skill_name)
    };

    let default_prompt = format!(
        "Review PR #{} in repo {}. Title: \"{}\". \
         Use {} to analyze changes, present each issue for approval, \
         and submit approved comments using gh CLI. Follow guidelines in {}",
        pr.number,
        pr.repo_name,
        pr.title.replace('"', "\\\""),
        skill_invocation,
        review_guide.display()
    );

    let prompt = ai
        .prompt_template
        .as_deref()
        .map(|template| render_prompt(template, pr, &review_guide, &skill_invocation))
        .unwrap_or(default_prompt);

    #[cfg(target_os = "macos")]
    {
        let workdir = unix_shell_escape(&working_dir.display().to_string());
        let cmd = build_unix_command(&command, &ai.args, &prompt);
        let command_line = format!("cd {} && {}", workdir, cmd);

        let terminal_app = ai
            .terminal_app
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Terminal");

        if terminal_app.eq_ignore_ascii_case("terminal") {
            launch_macos_terminal_applescript("Terminal", &command_line)?;
        } else {
            launch_macos_terminal_open(terminal_app, &command_line)?;
        }
    }

    #[cfg(target_os = "linux")]
    {
        let workdir = unix_shell_escape(&working_dir.display().to_string());
        let cmd = build_unix_command(&command, &ai.args, &prompt);
        let command_line = format!("cd {} && {}", workdir, cmd);
        let terminal_override = ai
            .terminal_app
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let mut terminals = Vec::new();
        if let Some(term) = terminal_override {
            terminals.push(term);
        }
        terminals.extend(["gnome-terminal", "konsole", "xterm"]);
        let mut launched = false;
        for term in terminals {
            let result = match term {
                "gnome-terminal" => Command::new(term)
                    .args(["--", "bash", "-c", &format!("{}; exec bash", command_line)])
                    .spawn(),
                "konsole" => Command::new(term)
                    .args(["-e", "bash", "-c", &format!("{}; exec bash", command_line)])
                    .spawn(),
                _ => Command::new(term)
                    .args(["-e", "bash", "-lc", &command_line])
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

    #[cfg(target_os = "windows")]
    {
        let display_name = ai.display_name();
        let workdir = windows_cmd_escape(&working_dir.display().to_string());
        let cmd = build_windows_command(&command, &ai.args, &prompt);
        let command_line = format!("cd /d {} && {}", workdir, cmd);
        Command::new("cmd")
            .args(["/C", "start", "cmd", "/K", &command_line])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("Failed to launch {}", display_name))?;
    }

    Ok(())
}
