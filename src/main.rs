mod config;
mod daemon;
mod diff;
mod gh;
mod harness;
mod repos;
mod terminal;
mod tui;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use rayon::prelude::*;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// TUI for reviewing GitHub PRs across multiple repositories
#[derive(Parser)]
#[command(name = "reviewer")]
#[command(
    version = env!("REVIEWER_VERSION_STRING"),
    about,
    long_about = None
)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Include draft PRs in the list
    #[arg(short, long)]
    drafts: bool,

    /// Show my PRs instead of PRs to review
    #[arg(short, long)]
    my: bool,

    /// Override the repos root directory
    #[arg(short, long)]
    root: Option<PathBuf>,

    /// Exclude directories (relative to root). Can be specified multiple times.
    /// Use --save-exclude to persist to config.
    #[arg(short, long)]
    exclude: Vec<String>,

    /// Save excluded directories to config
    #[arg(long)]
    save_exclude: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Run or configure the background daemon for auto reviews
    Daemon(DaemonArgs),
    /// Run terminal launch harness and write process evidence report
    Harness(harness::HarnessArgs),
}

#[derive(Parser)]
struct DaemonArgs {
    #[command(subcommand)]
    command: Option<DaemonCommand>,
}

#[derive(Subcommand, Clone, Copy)]
enum DaemonCommand {
    /// First-time setup: select excluded repos and seed already-open PRs
    Init,
    /// Run daemon loop
    Run {
        /// Run one polling cycle and exit
        #[arg(long)]
        once: bool,
        /// Override poll interval in seconds for this run
        #[arg(long, value_name = "SECONDS")]
        interval: Option<u64>,
    },
    /// Show daemon status and counters
    Status,
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

pub fn fetch_my_prs(include_drafts: bool) -> Vec<gh::PullRequest> {
    // Use gh search prs for a single API call across all repos
    gh::search_my_prs(include_drafts)
}

fn merge_excludes(config_exclude: &[String], cli_exclude: &[String]) -> Vec<String> {
    let mut exclude = config_exclude.to_vec();
    for value in cli_exclude {
        if !exclude.contains(value) {
            exclude.push(value.clone());
        }
    }
    exclude
}

fn validate_repos_root(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("Path does not exist: {}", path.display());
    }
    if !path.is_dir() {
        bail!("Path is not a directory: {}", path.display());
    }
    Ok(())
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

        if let Err(err) = validate_repos_root(&expanded) {
            println!("{}", err);
            continue;
        }

        return Ok(expanded);
    }
}

fn resolve_repos_root(cfg: &mut config::Config, root_override: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(root) = root_override {
        validate_repos_root(&root)?;
        return Ok(root);
    }

    if let Some(root) = &cfg.repos_root {
        let path = PathBuf::from(root);
        if !path.exists() {
            bail!(
                "Configured repos root no longer exists: {}. Delete {:?} to reconfigure.",
                root,
                config::config_path()
            );
        }
        return Ok(path);
    }

    let path = prompt_for_repos_root()?;
    cfg.repos_root = Some(path.to_string_lossy().to_string());
    config::save_config(cfg)?;
    println!("\nSaved repos root: {}\n", path.display());
    Ok(path)
}

fn run_tui(
    cfg: &config::Config,
    repos_root: PathBuf,
    username: String,
    include_drafts: bool,
    my_mode: bool,
    exclude: &[String],
) -> Result<()> {
    // Find repos
    println!("Scanning for repos in: {}", repos_root.display());
    if !exclude.is_empty() {
        println!("Excluding: {}", exclude.join(", "));
    }
    let scan = repos::scan_unique_repos(&repos_root, 3, exclude);
    let discovered_count = scan.discovered_count;
    let unique_count = scan.unique_repos.len();
    let duplicate_count = scan.duplicates_skipped();
    let repo_list: Vec<PathBuf> = scan
        .unique_repos
        .into_iter()
        .map(|repo| repo.path)
        .collect();
    if duplicate_count > 0 {
        println!(
            "Found {} repositories ({} unique, {} duplicates skipped)",
            discovered_count, unique_count, duplicate_count
        );
    } else {
        println!("Found {} repositories", unique_count);
    }

    if repo_list.is_empty() {
        println!("No repositories found.");
        return Ok(());
    }

    // Launch TUI immediately - it will fetch PRs in background
    println!("Launching TUI...");
    let mode = if my_mode {
        tui::AppMode::MyPrs
    } else {
        tui::AppMode::Review
    };
    tui::run(
        repos_root,
        repo_list,
        username,
        include_drafts,
        cfg.ai.clone(),
        mode,
    )?;

    Ok(())
}

fn print_daemon_status(cfg: &config::Config) {
    let status = daemon::status(cfg);
    println!("Daemon initialized: {}", status.initialized);
    println!("Poll interval: {}s", status.poll_interval_sec);
    println!("Include drafts: {}", status.include_drafts);
    println!("State file: {}", status.state_path.display());
    println!("Tracked PRs: {}", status.reviewed_count);
    println!("  Triggered successfully: {}", status.success_count);
    println!("  Failed to trigger: {}", status.failed_count);
    println!("  Seeded (already open on init): {}", status.seeded_count);
    if let Some(last_poll) = status.last_poll_at {
        println!("Last poll: {}", last_poll);
    } else {
        println!("Last poll: never");
    }
    if status.excluded_repos.is_empty() {
        println!("Excluded repos: none");
    } else {
        println!("Excluded repos ({}):", status.excluded_repos.len());
        for repo in status.excluded_repos {
            println!("  - {}", repo);
        }
    }
    if status.repo_subpath_filters.is_empty() {
        println!("Repo subpath filters: none");
    } else {
        println!(
            "Repo subpath filters ({}):",
            status.repo_subpath_filters.len()
        );
        for filter in status.repo_subpath_filters {
            println!("  - {}: {}", filter.repo, filter.subpaths.join(", "));
        }
    }
}

fn run_daemon_command(
    cfg: &mut config::Config,
    root_override: Option<PathBuf>,
    effective_exclude: Vec<String>,
    daemon_args: DaemonArgs,
) -> Result<()> {
    cfg.exclude = effective_exclude;
    let command = daemon_args.command.unwrap_or(DaemonCommand::Run {
        once: false,
        interval: None,
    });

    match command {
        DaemonCommand::Status => {
            print_daemon_status(cfg);
            Ok(())
        }
        DaemonCommand::Init => {
            let username = gh::get_current_user()?;
            println!("Authenticated as: {}\n", username);
            let repos_root = resolve_repos_root(cfg, root_override)?;
            daemon::init(cfg, &repos_root, &username)
        }
        DaemonCommand::Run { once, interval } => {
            let username = gh::get_current_user()?;
            println!("Authenticated as: {}\n", username);
            let repos_root = resolve_repos_root(cfg, root_override)?;
            if !cfg.daemon.initialized {
                println!("Daemon not initialized. Starting first-time setup...");
                daemon::init(cfg, &repos_root, &username)?;
            }
            daemon::run(cfg, &repos_root, &username, interval, once)
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    let mut cfg = config::load_config();
    let effective_exclude = merge_excludes(&cfg.exclude, &args.exclude);
    if args.save_exclude && !args.exclude.is_empty() {
        cfg.exclude = effective_exclude.clone();
        config::save_config(&cfg)?;
        println!("Saved exclusions to config: {:?}", args.exclude);
    }

    match args.command {
        Some(Commands::Daemon(daemon_args)) => {
            run_daemon_command(&mut cfg, args.root, effective_exclude, daemon_args)
        }
        Some(Commands::Harness(harness_args)) => harness::run(harness_args),
        None => {
            let username = gh::get_current_user()?;
            println!("Authenticated as: {}\n", username);
            let repos_root = resolve_repos_root(&mut cfg, args.root)?;
            run_tui(
                &cfg,
                repos_root,
                username,
                args.drafts,
                args.my,
                &effective_exclude,
            )
        }
    }
}
