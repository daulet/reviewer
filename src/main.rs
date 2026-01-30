mod config;
mod gh;
mod repos;
mod tui;

use anyhow::Result;
use clap::Parser;
use rayon::prelude::*;
use std::io::{self, Write};
use std::path::PathBuf;

/// TUI for reviewing GitHub PRs across multiple repositories
#[derive(Parser)]
#[command(name = "reviewer")]
#[command(version, about, long_about = None)]
struct Args {
    /// Include draft PRs in the list
    #[arg(short, long)]
    drafts: bool,

    /// Override the repos root directory
    #[arg(short, long)]
    root: Option<PathBuf>,
}

pub fn fetch_all_prs(
    repo_list: &[PathBuf],
    username: &str,
    include_drafts: bool,
) -> Vec<gh::PullRequest> {
    let all_prs: Vec<_> = repo_list
        .par_iter()
        .flat_map(|repo| gh::fetch_prs_for_repo(repo, username, include_drafts))
        .collect();

    // Sort by most recent first
    let mut all_prs = all_prs;
    all_prs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    all_prs
}

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
    let args = Args::parse();

    // Get current GitHub user
    let username = gh::get_current_user()?;
    println!("Authenticated as: {}\n", username);

    // Determine repos root: CLI arg > config > prompt
    let mut cfg = config::load_config();
    let repos_root = if let Some(root) = args.root {
        if !root.exists() {
            eprintln!("Error: Specified path does not exist: {}", root.display());
            std::process::exit(1);
        }
        root
    } else {
        match &cfg.repos_root {
            Some(root) => {
                let path = PathBuf::from(root);
                if !path.exists() {
                    eprintln!("Error: Configured repos root no longer exists: {}", root);
                    eprintln!("Delete {:?} to reconfigure.", config::config_path());
                    std::process::exit(1);
                }
                path
            }
            None => {
                let path = prompt_for_repos_root()?;
                cfg.repos_root = Some(path.to_string_lossy().to_string());
                config::save_config(&cfg)?;
                println!("\nSaved repos root: {}\n", path.display());
                path
            }
        }
    };

    // Find repos
    println!("Scanning for repos in: {}", repos_root.display());
    let repo_list = repos::find_repos(&repos_root, 3);
    println!("Found {} repositories", repo_list.len());

    if repo_list.is_empty() {
        println!("No repositories found.");
        return Ok(());
    }

    // Launch TUI immediately - it will fetch PRs in background
    println!("Launching TUI...");
    tui::run(repos_root, repo_list, username, args.drafts)?;

    Ok(())
}
