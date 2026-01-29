use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use walkdir::WalkDir;

// ============================================================================
// Config
// ============================================================================

#[derive(Debug, Default, Serialize, Deserialize)]
struct Config {
    repos_root: Option<String>,
}

fn config_path() -> PathBuf {
    // Use ~/.config for XDG-style paths (cross-platform consistency)
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("reviewer")
        .join("config.json")
}

fn load_config() -> Config {
    let path = config_path();
    if !path.exists() {
        return Config::default();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(config: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(config)?;
    std::fs::write(&path, json)?;
    Ok(())
}

// ============================================================================
// GitHub CLI
// ============================================================================

#[derive(Debug, Deserialize)]
struct RepoInfo {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

#[derive(Debug, Deserialize)]
struct Author {
    login: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Review {
    author: Option<Author>,
    state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PrData {
    number: u64,
    title: String,
    author: Option<Author>,
    url: String,
    #[serde(rename = "updatedAt")]
    updated_at: DateTime<Utc>,
    additions: Option<u64>,
    deletions: Option<u64>,
    reviews: Option<Vec<Review>>,
}

#[derive(Debug)]
struct PullRequest {
    number: u64,
    title: String,
    author: String,
    repo_name: String,
    #[allow(dead_code)]
    url: String,
    updated_at: DateTime<Utc>,
    additions: u64,
    deletions: u64,
}

fn get_current_user() -> Result<String> {
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
            "number,title,author,url,updatedAt,additions,deletions,reviews",
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

fn fetch_prs_for_repo(repo_path: &PathBuf, username: &str) -> Vec<PullRequest> {
    let repo_info = match get_repo_info(repo_path) {
        Some(info) => info,
        None => return Vec::new(),
    };

    let prs_data = get_open_prs(repo_path);
    let mut prs = Vec::new();

    for pr_data in prs_data {
        // Skip PRs already approved by user
        if has_user_approved(&pr_data, username) {
            continue;
        }

        // Skip user's own PRs
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
            repo_name: repo_info.name_with_owner.clone(),
            url: pr_data.url,
            updated_at: pr_data.updated_at,
            additions: pr_data.additions.unwrap_or(0),
            deletions: pr_data.deletions.unwrap_or(0),
        });
    }

    prs
}

// ============================================================================
// Repo Discovery
// ============================================================================

fn is_git_repo(path: &PathBuf) -> bool {
    path.join(".git").is_dir()
}

fn find_repos(root: &PathBuf, max_depth: usize) -> Vec<PathBuf> {
    let mut repos = Vec::new();

    for entry in WalkDir::new(root)
        .max_depth(max_depth)
        .into_iter()
        .filter_entry(|e| {
            // Skip hidden directories (except .git check happens after)
            !e.file_name()
                .to_str()
                .map(|s| s.starts_with('.'))
                .unwrap_or(false)
                || e.depth() == 0
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path().to_path_buf();
        if path.is_dir() && is_git_repo(&path) {
            repos.push(path);
        }
    }

    repos.sort();
    repos
}

// ============================================================================
// Main
// ============================================================================

fn prompt_for_repos_root() -> Result<PathBuf> {
    println!("First time setup: Please enter the root directory containing your repos.");
    println!("Example: ~/dev or /Users/you/projects");
    println!();

    loop {
        print!("Repos root directory: ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            println!("Please enter a path.");
            continue;
        }

        // Handle tilde expansion
        let expanded = if input.starts_with("~/") {
            dirs::home_dir()
                .map(|h| h.join(&input[2..]))
                .unwrap_or_else(|| PathBuf::from(input))
        } else {
            PathBuf::from(input)
        };

        if !expanded.exists() {
            println!("Path does not exist: {}", expanded.display());
            continue;
        }

        if !expanded.is_dir() {
            println!("Path is not a directory: {}", expanded.display());
            continue;
        }

        return Ok(expanded);
    }
}

fn main() -> Result<()> {
    // Get current GitHub user
    let username = get_current_user()?;
    println!("Authenticated as: {}\n", username);

    // Load or prompt for config
    let mut config = load_config();
    let repos_root = match &config.repos_root {
        Some(root) => {
            let path = PathBuf::from(root);
            if !path.exists() {
                eprintln!("Error: Configured repos root no longer exists: {}", root);
                eprintln!("Delete {:?} to reconfigure.", config_path());
                std::process::exit(1);
            }
            path
        }
        None => {
            let path = prompt_for_repos_root()?;
            config.repos_root = Some(path.to_string_lossy().to_string());
            save_config(&config)?;
            println!("\nSaved repos root: {}\n", path.display());
            path
        }
    };

    // Find repos
    println!("Scanning for repos in: {}", repos_root.display());
    let repos = find_repos(&repos_root, 3);
    println!("Found {} repositories\n", repos.len());

    if repos.is_empty() {
        println!("No repositories found.");
        return Ok(());
    }

    // Fetch PRs from all repos
    let mut all_prs: Vec<PullRequest> = Vec::new();
    for repo in &repos {
        let name = repo.file_name().unwrap_or_default().to_string_lossy();
        print!("  Checking {}...", name);
        io::stdout().flush()?;
        let prs = fetch_prs_for_repo(repo, &username);
        println!(" {} PRs", prs.len());
        all_prs.extend(prs);
    }

    // Sort by most recent first
    all_prs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    // Display results
    println!("\n{}", "=".repeat(60));
    println!("PRs requiring your attention: {}", all_prs.len());
    println!("{}\n", "=".repeat(60));

    if all_prs.is_empty() {
        println!("No PRs need your attention. You're all caught up!");
    } else {
        for (i, pr) in all_prs.iter().enumerate() {
            let stats = format!("+{}/-{}", pr.additions, pr.deletions);
            let date = pr.updated_at.format("%Y-%m-%d");
            println!(
                "{:3}. [{}] #{}: {}",
                i + 1,
                pr.repo_name,
                pr.number,
                pr.title
            );
            println!("      by @{} | {} | {}\n", pr.author, stats, date);
        }
    }

    Ok(())
}
