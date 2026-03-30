mod config;
mod daemon;
mod diff;
mod gh;
mod harness;
mod repos;
mod terminal;
mod tui;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rayon::prelude::*;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// TUI for reviewing GitHub PRs across multiple repositories
#[derive(Parser)]
#[command(name = "reviewer")]
#[command(
    version = env!("REVIEWER_VERSION_STRING"),
    disable_version_flag = true,
    about,
    long_about = None
)]
struct Args {
    /// Print version
    #[arg(
        short = 'v',
        short_alias = 'V',
        long = "version",
        action = clap::ArgAction::SetTrue,
        global = true
    )]
    version: bool,

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
    /// Trigger an AI review session for a specific PR
    Trigger(TriggerArgs),
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

#[derive(Parser)]
struct TriggerArgs {
    /// PR number to trigger
    #[arg(long, value_name = "NUMBER")]
    pr: u64,
    /// Target repository in owner/name format
    #[arg(long, value_name = "OWNER/REPO", required_unless_present = "repo_path")]
    repo: Option<String>,
    /// Local path to the repo clone (skips repo scan)
    #[arg(long, value_name = "PATH", required_unless_present = "repo")]
    repo_path: Option<PathBuf>,
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
    println!(
        "Only new PRs on first run: {}",
        status.only_new_prs_on_start
    );
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
    if status.auto_approve_rules.is_empty() {
        println!("Auto-approve rules: none");
    } else {
        println!("Auto-approve rules ({}):", status.auto_approve_rules.len());
        for rule in status.auto_approve_rules {
            println!("  - {} @{}", rule.repo, rule.user);
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

fn resolve_trigger_repo(
    repos_root: &Path,
    effective_exclude: &[String],
    args: &TriggerArgs,
) -> Result<(PathBuf, String)> {
    if let Some(repo_path) = &args.repo_path {
        if !repo_path.exists() {
            bail!("Repo path does not exist: {}", repo_path.display());
        }
        if !repo_path.is_dir() {
            bail!("Repo path is not a directory: {}", repo_path.display());
        }

        let repo_name = gh::repo_name_with_owner(repo_path).with_context(|| {
            format!(
                "Failed to resolve owner/name via gh for repo path {}",
                repo_path.display()
            )
        })?;

        if let Some(expected_repo) = &args.repo {
            if expected_repo != &repo_name {
                bail!(
                    "Repo mismatch: --repo={} but --repo-path resolves to {}",
                    expected_repo,
                    repo_name
                );
            }
        }

        return Ok((repo_path.clone(), repo_name));
    }

    let repo_name = args
        .repo
        .as_ref()
        .context("Either --repo or --repo-path must be provided")?;

    let tail = repo_name.rsplit('/').next().unwrap_or(repo_name);
    let direct_candidate = repos_root.join(tail);
    if direct_candidate.is_dir() && direct_candidate.join(".git").exists() {
        match gh::repo_name_with_owner(&direct_candidate) {
            Some(name_with_owner) if name_with_owner != *repo_name => {}
            _ => return Ok((direct_candidate, repo_name.clone())),
        }
    }

    println!(
        "Resolving local clone for {} under {}...",
        repo_name,
        repos_root.display()
    );

    let scan = repos::scan_unique_repos(repos_root, 3, effective_exclude);
    let repo = scan
        .unique_repos
        .into_iter()
        .find(|repo| repo.name_with_owner.as_deref() == Some(repo_name.as_str()));

    if let Some(repo) = repo {
        return Ok((repo.path, repo_name.clone()));
    }

    bail!(
        "Could not find local clone for {} under {}. Use --repo-path to specify it directly.",
        repo_name,
        repos_root.display()
    );
}

fn run_trigger_command(
    cfg: &mut config::Config,
    root_override: Option<PathBuf>,
    effective_exclude: Vec<String>,
    trigger_args: TriggerArgs,
) -> Result<()> {
    let repos_root = resolve_repos_root(cfg, root_override)?;
    let (repo_path, repo_name) =
        resolve_trigger_repo(&repos_root, &effective_exclude, &trigger_args)?;

    println!("Triggering review for {}#{}...", repo_name, trigger_args.pr);

    let pr = gh::fetch_pr_for_review(&repo_path, &repo_name, trigger_args.pr)?;
    gh::validate_ai_launch_config(&cfg.ai)?;

    let worktree_path = gh::create_pr_worktree(&pr, &repos_root).with_context(|| {
        format!(
            "Failed to create worktree for {}#{}",
            repo_name, trigger_args.pr
        )
    })?;
    gh::launch_ai(&worktree_path, &pr, &cfg.ai).with_context(|| {
        format!(
            "Failed to launch review for {}#{}",
            repo_name, trigger_args.pr
        )
    })?;

    println!(
        "Triggered review for {}#{} (worktree: {})",
        repo_name,
        trigger_args.pr,
        worktree_path.display()
    );
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.version {
        println!("{}", env!("REVIEWER_VERSION_STRING"));
        return Ok(());
    }

    let mut cfg = config::load_config()?;
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
        Some(Commands::Trigger(trigger_args)) => {
            run_trigger_command(&mut cfg, args.root, effective_exclude, trigger_args)
        }
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

#[cfg(test)]
mod tests {
    use super::Args;
    use clap::Parser;

    #[test]
    fn daemon_run_parses_without_version_flag() {
        let parsed = Args::try_parse_from(["reviewer", "daemon", "run", "--interval", "300"]);
        assert!(parsed.is_ok());
    }

    #[test]
    fn trigger_parses_with_repo_and_pr() {
        let parsed =
            Args::try_parse_from(["reviewer", "trigger", "--repo", "org/repo", "--pr", "2398"]);
        assert!(parsed.is_ok());
    }

    #[test]
    fn trigger_parses_with_repo_path_and_pr() {
        let parsed = Args::try_parse_from([
            "reviewer",
            "trigger",
            "--repo-path",
            "/tmp/repo",
            "--pr",
            "2398",
        ]);
        assert!(parsed.is_ok());
    }
}
