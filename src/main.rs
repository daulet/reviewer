mod config;
mod gh;
mod repos;
mod tui;

use anyhow::Result;
use rayon::prelude::*;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    // Get current GitHub user
    let username = gh::get_current_user()?;
    println!("Authenticated as: {}\n", username);

    // Load or prompt for config
    let mut cfg = config::load_config();
    let repos_root = match &cfg.repos_root {
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
    };

    // Find repos
    println!("Scanning for repos in: {}", repos_root.display());
    let repo_list = repos::find_repos(&repos_root, 3);
    println!("Found {} repositories\n", repo_list.len());

    if repo_list.is_empty() {
        println!("No repositories found.");
        return Ok(());
    }

    // Fetch PRs from all repos in parallel
    println!("Fetching PRs from {} repositories...", repo_list.len());
    let completed = AtomicUsize::new(0);
    let total = repo_list.len();

    let all_prs: Vec<_> = repo_list
        .par_iter()
        .flat_map(|repo| {
            let prs = gh::fetch_prs_for_repo(repo, &username);
            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            eprint!("\r  Progress: {}/{}          ", done, total);
            let _ = io::stderr().flush();
            prs
        })
        .collect();

    eprintln!("\r  Checked {} repositories.   ", total);

    // Sort by most recent first
    let mut all_prs = all_prs;
    all_prs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    println!("\nFound {} PRs requiring review. Launching TUI...", all_prs.len());

    if all_prs.is_empty() {
        println!("No PRs need your attention. You're all caught up!");
        return Ok(());
    }

    // Launch TUI
    tui::run(all_prs, repos_root)?;

    Ok(())
}
