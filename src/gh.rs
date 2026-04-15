use crate::config::{self, AiConfig};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;

const DEFAULT_PR_LIST_LIMIT: usize = 100;
const FIRST_PAGE_PR_LIST_LIMIT: usize = 30;

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

/// PR data from the global GraphQL PR search.
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

#[derive(Debug, Deserialize)]
struct SearchResponse {
    data: Option<SearchData>,
}

#[derive(Debug, Deserialize)]
struct SearchData {
    search: SearchNodes,
}

#[derive(Debug, Deserialize)]
struct SearchNodes {
    nodes: Vec<SearchPrData>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Debug, Deserialize)]
struct PageInfo {
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoPrFetchMode {
    ReviewCandidates,
    ReviewAndSelfCandidates,
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
    pub details_loaded: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PullRequestPage {
    pub prs: Vec<PullRequest>,
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
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

pub fn repo_name_with_owner(repo_path: &PathBuf) -> Option<String> {
    get_repo_info(repo_path).map(|info| info.name_with_owner)
}

fn get_open_prs(repo_path: &PathBuf, limit: usize) -> Vec<PrData> {
    let limit_arg = limit.to_string();
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--json",
            "number,title,author,body,url,updatedAt,additions,deletions,reviews,isDraft,reviewDecision",
            "--limit",
        ])
        .arg(&limit_arg)
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

fn review_state_from_fields(is_draft: bool, review_decision: Option<&str>) -> ReviewState {
    if is_draft {
        return ReviewState::Draft;
    }

    match review_decision {
        Some("APPROVED") => ReviewState::Approved,
        Some("CHANGES_REQUESTED") => ReviewState::ChangesRequested,
        _ => ReviewState::Pending,
    }
}

fn determine_review_state(pr_data: &PrData) -> ReviewState {
    review_state_from_fields(
        pr_data.is_draft.unwrap_or(false),
        pr_data.review_decision.as_deref(),
    )
}

fn pr_data_to_pull_request(pr_data: PrData, repo_path: PathBuf, repo_name: String) -> PullRequest {
    let pr_author = pr_data
        .author
        .as_ref()
        .and_then(|a| a.login.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("unknown");
    let review_state = determine_review_state(&pr_data);

    PullRequest {
        number: pr_data.number,
        title: pr_data.title,
        author: pr_author.to_string(),
        body: pr_data.body.unwrap_or_default(),
        repo_path,
        repo_name,
        url: pr_data.url,
        updated_at: pr_data.updated_at,
        additions: pr_data.additions.unwrap_or(0),
        deletions: pr_data.deletions.unwrap_or(0),
        is_draft: pr_data.is_draft.unwrap_or(false),
        review_state,
        details_loaded: true,
    }
}

fn search_pr_data_to_pull_request(pr_data: SearchPrData) -> PullRequest {
    let pr_author = pr_data
        .author
        .as_ref()
        .and_then(|a| a.login.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("unknown");
    let is_draft = pr_data.is_draft.unwrap_or(false);
    let review_state = review_state_from_fields(is_draft, None);

    PullRequest {
        number: pr_data.number,
        title: pr_data.title,
        author: pr_author.to_string(),
        body: pr_data.body.unwrap_or_default(),
        repo_path: PathBuf::new(),
        repo_name: pr_data.repository.name_with_owner,
        url: pr_data.url,
        updated_at: pr_data.updated_at,
        additions: 0,
        deletions: 0,
        is_draft,
        review_state,
        details_loaded: false,
    }
}

fn repo_name_from_pr_url(url: &str) -> Option<String> {
    let mut segments = url.split('/');
    let _scheme = segments.next()?;
    let _empty = segments.next()?;
    let _host = segments.next()?;
    let owner = segments.next()?;
    let repo = segments.next()?;
    Some(format!("{owner}/{repo}"))
}

fn fetch_prs_for_repo_with_mode(
    repo_path: &PathBuf,
    username: &str,
    include_drafts: bool,
    mode: RepoPrFetchMode,
    limit: usize,
) -> Vec<PullRequest> {
    let prs_data = get_open_prs(repo_path, limit);
    if prs_data.is_empty() {
        return Vec::new();
    }

    let mut repo_name_fallback: Option<String> = None;
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

        if mode == RepoPrFetchMode::ReviewCandidates && pr_author == username {
            continue;
        }

        let repo_name = repo_name_from_pr_url(&pr_data.url).or_else(|| {
            if repo_name_fallback.is_none() {
                repo_name_fallback = get_repo_info(repo_path).map(|info| info.name_with_owner);
            }
            repo_name_fallback.clone()
        });
        let Some(repo_name) = repo_name else {
            continue;
        };

        prs.push(pr_data_to_pull_request(
            pr_data,
            repo_path.clone(),
            repo_name,
        ));
    }

    prs
}

pub fn fetch_prs_for_repo_with_authored(
    repo_path: &PathBuf,
    username: &str,
    include_drafts: bool,
) -> Vec<PullRequest> {
    fetch_prs_for_repo_with_mode(
        repo_path,
        username,
        include_drafts,
        RepoPrFetchMode::ReviewAndSelfCandidates,
        DEFAULT_PR_LIST_LIMIT,
    )
}

/// Fetch a specific PR directly, bypassing list-mode filtering (draft/approved checks).
pub fn fetch_pr_for_review(
    repo_path: &PathBuf,
    repo_name: &str,
    pr_number: u64,
) -> Result<PullRequest> {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--repo",
            repo_name,
            "--json",
            "number,title,author,body,url,updatedAt,additions,deletions,isDraft,reviewDecision",
        ])
        .current_dir(repo_path)
        .output()
        .context("Failed to fetch PR details")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to fetch PR details for {}#{}: {}",
            repo_name,
            pr_number,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let pr_data: PrData = serde_json::from_slice(&output.stdout).context(format!(
        "Failed to parse PR details response for {}#{}",
        repo_name, pr_number
    ))?;

    Ok(pr_data_to_pull_request(
        pr_data,
        repo_path.clone(),
        repo_name.to_string(),
    ))
}

pub fn fetch_pr_details(pr: &PullRequest) -> Result<PullRequest> {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            &pr.number.to_string(),
            "--repo",
            &pr.repo_name,
            "--json",
            "number,title,author,body,url,updatedAt,additions,deletions,isDraft,reviewDecision",
        ])
        .output()
        .context("Failed to fetch PR details")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to fetch PR details for {}#{}: {}",
            pr.repo_name,
            pr.number,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let pr_data: PrData = serde_json::from_slice(&output.stdout).context(format!(
        "Failed to parse PR details response for {}#{}",
        pr.repo_name, pr.number
    ))?;

    Ok(pr_data_to_pull_request(
        pr_data,
        pr.repo_path.clone(),
        pr.repo_name.clone(),
    ))
}

#[derive(Debug, Deserialize)]
struct PrFileData {
    path: String,
}

#[derive(Debug, Deserialize)]
struct PrFilesData {
    files: Option<Vec<PrFileData>>,
}

pub fn get_pr_changed_files(pr: &PullRequest) -> Result<Vec<String>> {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            &pr.number.to_string(),
            "--repo",
            &pr.repo_name,
            "--json",
            "files",
        ])
        .output()
        .context("Failed to get PR changed files")?;

    if !output.status.success() {
        anyhow::bail!(
            "Failed to get PR changed files: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let data: PrFilesData = serde_json::from_slice(&output.stdout)
        .context("Failed to parse PR changed files response")?;
    Ok(data
        .files
        .unwrap_or_default()
        .into_iter()
        .map(|file| file.path)
        .collect())
}

#[derive(Debug, Clone, Copy)]
enum SearchScope {
    Involved,
    Authored,
}

/// Search for the first page of open PRs involving the current user.
pub fn search_involved_prs(
    username: &str,
    include_drafts: bool,
    after: Option<&str>,
) -> PullRequestPage {
    search_prs_with_limit(
        username,
        include_drafts,
        FIRST_PAGE_PR_LIST_LIMIT,
        SearchScope::Involved,
        after,
    )
}

/// Search for the first page of open PRs authored by the current user.
pub fn search_my_prs(username: &str, include_drafts: bool, after: Option<&str>) -> PullRequestPage {
    search_prs_with_limit(
        username,
        include_drafts,
        FIRST_PAGE_PR_LIST_LIMIT,
        SearchScope::Authored,
        after,
    )
}

fn search_prs_with_limit(
    username: &str,
    include_drafts: bool,
    limit: usize,
    scope: SearchScope,
    after: Option<&str>,
) -> PullRequestPage {
    let mut qualifiers = vec!["is:pr".to_string(), "is:open".to_string()];
    qualifiers.push(match scope {
        SearchScope::Involved => format!("involves:{username}"),
        SearchScope::Authored => format!("author:{username}"),
    });
    if !include_drafts {
        qualifiers.push("draft:false".to_string());
    }
    qualifiers.push("sort:updated-desc".to_string());

    let search_query = qualifiers.join(" ");
    let query_literal = serde_json::to_string(&search_query).unwrap_or_default();
    let first = limit.min(100);
    let after_arg = after
        .and_then(|cursor| serde_json::to_string(cursor).ok())
        .map(|cursor| format!(", after: {cursor}"))
        .unwrap_or_default();
    let query = format!(
        r#"query {{
            search(query: {query_literal}, type: ISSUE, first: {first}{after_arg}) {{
                nodes {{
                    ... on PullRequest {{
                        number
                        title
                        author {{
                            login
                        }}
                        body
                        url
                        updatedAt
                        isDraft
                        repository {{
                            nameWithOwner
                        }}
                    }}
                }}
                pageInfo {{
                    endCursor
                    hasNextPage
                }}
            }}
        }}"#
    );
    let query_arg = format!("query={query}");

    let output = Command::new("gh")
        .args(["api", "graphql", "-f"])
        .arg(query_arg)
        .output()
        .ok();

    let response: SearchResponse = match output {
        Some(o) if o.status.success() => {
            serde_json::from_slice(&o.stdout).unwrap_or(SearchResponse { data: None })
        }
        _ => return PullRequestPage::default(),
    };

    response
        .data
        .map(|data| {
            let SearchNodes { nodes, page_info } = data.search;
            let prs = nodes
                .into_iter()
                .map(search_pr_data_to_pull_request)
                .collect();
            PullRequestPage {
                prs,
                end_cursor: page_info.end_cursor,
                has_next_page: page_info.has_next_page,
            }
        })
        .unwrap_or_default()
}

pub fn get_pr_diff(pr: &PullRequest) -> Result<String> {
    let output = Command::new("gh")
        .args([
            "pr",
            "diff",
            &pr.number.to_string(),
            "--repo",
            &pr.repo_name,
        ])
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
    if pr.repo_path.as_os_str().is_empty() {
        anyhow::bail!(
            "Diff is too large for gh to fetch directly and no local clone is associated with {}#{}",
            pr.repo_name,
            pr.number
        );
    }

    // Get the base and head commit SHAs
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            &pr.number.to_string(),
            "--repo",
            &pr.repo_name,
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
            "--repo",
            &pr.repo_name,
            "--json",
            "comments",
            "--jq",
            ".comments",
        ])
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
        .args([
            "pr",
            "comment",
            &pr.number.to_string(),
            "--repo",
            &pr.repo_name,
            "--body",
            comment,
        ])
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
    let mut args = vec![
        "pr",
        "review",
        &pr_number,
        "--repo",
        &pr.repo_name,
        "--approve",
    ];

    let body_arg;
    if let Some(c) = comment {
        body_arg = c.to_string();
        args.push("--body");
        args.push(&body_arg);
    }

    let output = Command::new("gh")
        .args(&args)
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
    let repo_path = resolve_worktree_repo_path(pr, repos_root)?;

    let worktree_name = format!("{}-pr-{}", pr.repo_name.replace('/', "-"), pr.number);
    let canonical_path = worktree_base.join(&worktree_name);
    cleanup_worktree_path(&repo_path, &canonical_path);

    // Fetch the PR head ref
    let pr_ref = format!("refs/pull/{}/head", pr.number);
    let fetch_output = Command::new("git")
        .args(["fetch", "origin", &pr_ref])
        .current_dir(&repo_path)
        .output()
        .context("Failed to fetch PR ref")?;

    if !fetch_output.status.success() {
        anyhow::bail!(
            "Failed to fetch PR: {}",
            String::from_utf8_lossy(&fetch_output.stderr)
        );
    }

    // Prefer canonical path, then fall back to timestamp-suffixed paths when a previous
    // worktree is still active or metadata is stale.
    let mut candidates = vec![canonical_path];
    let stamp = Utc::now().format("%Y%m%d%H%M%S");
    for attempt in 1..=5 {
        candidates.push(worktree_base.join(format!("{worktree_name}-{stamp}-{attempt}")));
    }

    let mut errors = Vec::new();
    for candidate in candidates {
        if candidate.exists() {
            cleanup_worktree_path(&repo_path, &candidate);
        }

        match git_worktree_add(&repo_path, &candidate, "FETCH_HEAD") {
            Ok(()) => return Ok(candidate),
            Err(err) => errors.push(format!("{} => {}", candidate.display(), err)),
        }
    }

    anyhow::bail!(
        "Failed to create worktree for {}#{} after trying multiple paths:\n{}",
        pr.repo_name,
        pr.number,
        errors.join("\n")
    );
}

fn resolve_worktree_repo_path(
    pr: &PullRequest,
    repos_root: &std::path::Path,
) -> Result<std::path::PathBuf> {
    if !pr.repo_path.as_os_str().is_empty() {
        return Ok(pr.repo_path.clone());
    }

    for candidate in worktree_repo_path_candidates(&pr.repo_name, repos_root) {
        if !candidate.join(".git").exists() {
            continue;
        }

        match repo_name_with_owner(&candidate) {
            Some(name) if name == pr.repo_name => return Ok(candidate),
            Some(_) => {}
            None => {}
        }
    }

    anyhow::bail!(
        "No local clone found for {} under {}. Use `reviewer trigger --repo-path` for PRs that need a worktree.",
        pr.repo_name,
        repos_root.display()
    );
}

fn worktree_repo_path_candidates(repo_name: &str, repos_root: &std::path::Path) -> Vec<PathBuf> {
    let mut parts = repo_name.split('/');
    let Some(owner) = parts.next() else {
        return Vec::new();
    };
    let Some(name) = parts.next() else {
        return Vec::new();
    };

    let mut candidates = vec![
        repos_root.join(name),
        repos_root.join(owner).join(name),
        repos_root.join(repo_name.replace('/', "-")),
    ];
    candidates.sort();
    candidates.dedup();
    candidates
}

fn cleanup_worktree_path(repo_path: &std::path::Path, worktree_path: &std::path::Path) {
    let _ = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(worktree_path)
        .current_dir(repo_path)
        .output();
    let _ = Command::new("git")
        .args(["worktree", "prune"])
        .current_dir(repo_path)
        .output();
    let _ = std::fs::remove_dir_all(worktree_path);
}

fn git_worktree_add(
    repo_path: &std::path::Path,
    worktree_path: &std::path::Path,
    revision: &str,
) -> Result<()> {
    let output = Command::new("git")
        .args(["worktree", "add"])
        .arg(worktree_path)
        .arg(revision)
        .current_dir(repo_path)
        .output()
        .context("Failed to create worktree")?;

    if !output.status.success() {
        anyhow::bail!(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
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

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn build_shell_command(command: &str, args: &[String], prompt: &str) -> String {
    build_unix_command(command, args, prompt)
}

#[cfg(target_os = "windows")]
fn build_shell_command(command: &str, args: &[String], prompt: &str) -> String {
    build_windows_command(command, args, prompt)
}

fn command_error_message(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return stdout;
    }
    format!("exit status {}", output.status)
}

fn launch_session_title(pr: &PullRequest) -> String {
    let repo = pr.repo_name.replace('/', "-");
    format!(
        "review-{}-pr-{}-{}",
        repo,
        pr.number,
        Utc::now().timestamp_millis()
    )
}

struct LaunchTemplateValues {
    provider: String,
    repo: String,
    repo_slug: String,
    pr_number: String,
    title: String,
    prompt: String,
    review_guide: String,
    skill_name: String,
    skill_invocation: String,
    tool: String,
    tool_command: String,
    workdir: String,
    workdir_shell: String,
    session_title: String,
    timestamp_ms: String,
}

struct LaunchContext<'a> {
    working_dir: &'a std::path::Path,
    tool: &'a str,
    tool_args: &'a [String],
    prompt: &'a str,
    review_guide: &'a std::path::Path,
    pr: &'a PullRequest,
    provider: &'a str,
    skill_name: &'a str,
    skill_invocation: &'a str,
}

impl LaunchTemplateValues {
    fn from_context(context: LaunchContext<'_>) -> Self {
        let workdir = context.working_dir.display().to_string();
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let workdir_shell = unix_shell_escape(&workdir);
        #[cfg(target_os = "windows")]
        let workdir_shell = windows_cmd_escape(&workdir);

        Self {
            provider: context.provider.to_string(),
            repo: context.pr.repo_name.clone(),
            repo_slug: context.pr.repo_name.replace('/', "-"),
            pr_number: context.pr.number.to_string(),
            title: context.pr.title.clone(),
            prompt: context.prompt.to_string(),
            review_guide: context.review_guide.display().to_string(),
            skill_name: context.skill_name.to_string(),
            skill_invocation: context.skill_invocation.to_string(),
            tool: context.tool.to_string(),
            tool_command: build_shell_command(context.tool, context.tool_args, context.prompt),
            workdir,
            workdir_shell,
            session_title: launch_session_title(context.pr),
            timestamp_ms: Utc::now().timestamp_millis().to_string(),
        }
    }
}

fn render_launch_template(template: &str, values: &LaunchTemplateValues) -> String {
    let mut rendered = template.to_string();
    for (key, value) in [
        ("{skill_name}", values.skill_name.as_str()),
        ("{skill_invocation}", values.skill_invocation.as_str()),
        ("{session_title}", values.session_title.as_str()),
        ("{timestamp_ms}", values.timestamp_ms.as_str()),
        ("{workdir_shell}", values.workdir_shell.as_str()),
        ("{tool_command}", values.tool_command.as_str()),
        ("{provider}", values.provider.as_str()),
        ("{repo_slug}", values.repo_slug.as_str()),
        ("{repo}", values.repo.as_str()),
        ("{pr_number}", values.pr_number.as_str()),
        ("{title}", values.title.as_str()),
        ("{prompt}", values.prompt.as_str()),
        ("{review_guide}", values.review_guide.as_str()),
        ("{tool}", values.tool.as_str()),
        ("{workdir}", values.workdir.as_str()),
    ] {
        rendered = rendered.replace(key, value);
    }
    rendered
}

pub fn validate_ai_launch_config(ai: &AiConfig) -> Result<()> {
    if ai.launch.steps.is_empty() {
        anyhow::bail!(
            "ai.launch.steps is empty. Configure launcher commands in ~/.config/reviewer/config.json"
        );
    }

    for (idx, step) in ai.launch.steps.iter().enumerate() {
        if step.command.trim().is_empty() {
            anyhow::bail!("ai.launch.steps[{idx}] command is empty");
        }
    }

    Ok(())
}

fn launch_with_steps(
    working_dir: &std::path::Path,
    ai: &AiConfig,
    values: &LaunchTemplateValues,
) -> Result<()> {
    validate_ai_launch_config(ai)?;

    let total = ai.launch.steps.len();
    for (idx, step) in ai.launch.steps.iter().enumerate() {
        let step_number = idx + 1;
        let command = render_launch_template(&step.command, values);
        let command = command.trim();
        if command.is_empty() {
            anyhow::bail!("ai.launch.steps[{idx}] command is empty after template rendering");
        }
        let args: Vec<String> = step
            .args
            .iter()
            .map(|arg| render_launch_template(arg, values))
            .collect();

        let output = Command::new(command)
            .args(&args)
            .current_dir(working_dir)
            .output()
            .with_context(|| format!("Failed to run ai.launch step {step_number}/{total}"))?;
        if !output.status.success() {
            anyhow::bail!(
                "ai.launch step {step_number}/{total} failed ({command}): {}",
                command_error_message(&output)
            );
        }
    }

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
    let tool = ai.command_name();

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

    let values = LaunchTemplateValues::from_context(LaunchContext {
        working_dir,
        tool: &tool,
        tool_args: &ai.args,
        prompt: &prompt,
        review_guide: &review_guide,
        pr,
        provider,
        skill_name: &skill_name,
        skill_invocation: &skill_invocation,
    });
    launch_with_steps(working_dir, ai, &values)
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::{
        build_shell_command, launch_with_steps, render_launch_template, LaunchContext,
        LaunchTemplateValues, PullRequest,
    };
    use crate::config::AiConfig;
    use chrono::Utc;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    #[test]
    fn build_shell_command_escapes_special_characters() {
        let cmd = build_shell_command("printf", &[String::from("%s")], "it's $HOME");
        let output = Command::new("sh")
            .args(["-lc", &cmd])
            .output()
            .expect("command should execute");

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "it's $HOME");
    }

    fn make_test_pr(number: u64, title: &str, repo: &str) -> PullRequest {
        PullRequest {
            number,
            title: title.to_string(),
            author: "alice".to_string(),
            body: String::new(),
            repo_path: PathBuf::from("/tmp/repo"),
            repo_name: repo.to_string(),
            url: "https://example.com".to_string(),
            updated_at: Utc::now(),
            additions: 1,
            deletions: 1,
            is_draft: false,
            review_state: super::ReviewState::Pending,
            details_loaded: true,
        }
    }

    #[test]
    fn render_launch_template_replaces_placeholders() {
        let pr = make_test_pr(42, "Fix launch", "org/reviewer");
        let values = LaunchTemplateValues::from_context(LaunchContext {
            working_dir: Path::new("/tmp/repo"),
            tool: "codex",
            tool_args: &[],
            prompt: "Review this",
            review_guide: Path::new("/tmp/review_guide.md"),
            pr: &pr,
            provider: "codex",
            skill_name: "code-review",
            skill_invocation: "$code-review",
        });
        let rendered = render_launch_template(
            "{repo}|{pr_number}|{tool}|{prompt}|{skill_invocation}|{session_title}",
            &values,
        );
        assert!(rendered
            .contains("org/reviewer|42|codex|Review this|$code-review|review-org-reviewer-pr-42-"));
    }

    #[test]
    fn launch_with_steps_requires_non_empty_steps() {
        let pr = make_test_pr(1, "Title", "org/reviewer");
        let values = LaunchTemplateValues::from_context(LaunchContext {
            working_dir: Path::new("/tmp/repo"),
            tool: "codex",
            tool_args: &[],
            prompt: "Prompt",
            review_guide: Path::new("/tmp/review_guide.md"),
            pr: &pr,
            provider: "codex",
            skill_name: "code-review",
            skill_invocation: "$code-review",
        });
        let err = launch_with_steps(Path::new("/tmp/repo"), &AiConfig::default(), &values)
            .expect_err("expected launch config error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ai.launch.steps is empty"),
            "unexpected error: {msg}"
        );
    }
}
