mod config;
mod daemon;
mod diff;
mod filters;
mod gh;
mod harness;
mod repos;
mod terminal;
mod tui;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
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

    /// Override the local repos root used for worktrees and repo-scan commands
    #[arg(short, long)]
    root: Option<PathBuf>,

    /// Exclude directories from repo scans (relative to root). Can be specified multiple times.
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
    /// PR URL or shorthand, e.g. https://github.com/org/repo/pull/123 or org/repo#123
    #[arg(value_name = "PR")]
    target: Option<String>,
    /// PR number to trigger
    #[arg(long, value_name = "NUMBER")]
    pr: Option<u64>,
    /// Target repository in owner/name format
    #[arg(long, value_name = "OWNER/REPO")]
    repo: Option<String>,
    /// Local path to the repo clone (skips repo scan)
    #[arg(long, value_name = "PATH")]
    repo_path: Option<PathBuf>,
}

pub fn fetch_involved_prs(
    username: &str,
    include_drafts: bool,
    after: Option<&str>,
    exclude_users: &[String],
) -> gh::PullRequestPage {
    filter_excluded_pr_authors(
        gh::search_involved_prs(username, include_drafts, after, exclude_users),
        exclude_users,
    )
}

pub fn fetch_my_prs(
    username: &str,
    include_drafts: bool,
    after: Option<&str>,
    exclude_users: &[String],
) -> gh::PullRequestPage {
    filter_excluded_pr_authors(
        gh::search_my_prs(username, include_drafts, after, exclude_users),
        exclude_users,
    )
}

fn filter_excluded_pr_authors(
    mut page: gh::PullRequestPage,
    exclude_users: &[String],
) -> gh::PullRequestPage {
    let exclude_users = filters::normalize_user_patterns(exclude_users);
    if !exclude_users.is_empty() {
        page.prs.retain(|pr| {
            !filters::author_excluded(&pr.author, pr.author_kind.as_deref(), &exclude_users)
        });
    }
    page
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

fn resolve_tui_repos_root(cfg: &config::Config, root_override: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(root) = root_override {
        validate_repos_root(&root)?;
        return Ok(root);
    }

    if let Some(root) = &cfg.repos_root {
        let path = PathBuf::from(root);
        if path.exists() && path.is_dir() {
            return Ok(path);
        }
    }

    std::env::current_dir().context("Failed to resolve current directory")
}

fn run_tui(
    ai: config::AiConfig,
    repos_root: PathBuf,
    username: String,
    include_drafts: bool,
    my_mode: bool,
    exclude_users: Vec<String>,
) -> Result<()> {
    println!("Launching TUI...");
    let mode = if my_mode {
        tui::AppMode::MyPrs
    } else {
        tui::AppMode::Review
    };
    tui::run(
        repos_root,
        username,
        include_drafts,
        ai,
        mode,
        exclude_users,
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
    if status.excluded_users.is_empty() {
        println!("Excluded users: none");
    } else {
        println!("Excluded users ({}):", status.excluded_users.len());
        for user in status.excluded_users {
            println!("  - @{}", user);
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedTriggerTarget {
    repo: String,
    pr: u64,
}

fn parse_trigger_target(target: &str) -> Result<ParsedTriggerTarget> {
    let target = target.trim();
    let target = target
        .split('?')
        .next()
        .unwrap_or(target)
        .trim_end_matches('/');

    if let Some((repo, pr)) = target.rsplit_once('#') {
        return Ok(ParsedTriggerTarget {
            repo: parse_repo_name(repo)?,
            pr: parse_pr_number(pr)?,
        });
    }

    let path = target
        .strip_prefix("https://github.com/")
        .or_else(|| target.strip_prefix("http://github.com/"))
        .or_else(|| target.strip_prefix("github.com/"))
        .unwrap_or(target);

    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() == 4 && parts[2] == "pull" {
        return Ok(ParsedTriggerTarget {
            repo: parse_repo_name(&format!("{}/{}", parts[0], parts[1]))?,
            pr: parse_pr_number(parts[3])?,
        });
    }

    bail!(
        "Could not parse trigger target '{}'. Use a GitHub PR URL like https://github.com/org/repo/pull/123 or org/repo#123.",
        target
    )
}

fn parse_repo_name(repo: &str) -> Result<String> {
    let parts: Vec<&str> = repo.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() == 2 {
        Ok(format!("{}/{}", parts[0], parts[1]))
    } else {
        bail!("Repository must be in owner/name format, got '{}'", repo)
    }
}

fn parse_pr_number(value: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .with_context(|| format!("PR number must be numeric, got '{}'", value))
}

fn merge_trigger_repo_arg(
    positional: Option<String>,
    flag: Option<String>,
    has_repo_path: bool,
) -> Result<Option<String>> {
    match (positional, flag) {
        (Some(positional), Some(flag)) if positional != flag => {
            bail!(
                "Repo mismatch: positional target resolves to {} but --repo={}",
                positional,
                flag
            )
        }
        (Some(repo), _) | (_, Some(repo)) => Ok(Some(repo)),
        (None, None) if has_repo_path => Ok(None),
        (None, None) => bail!(
            "Target repo is required. Provide a PR URL, owner/repo#number, --repo, or --repo-path."
        ),
    }
}

fn merge_trigger_pr_arg(positional: Option<u64>, flag: Option<u64>) -> Result<u64> {
    match (positional, flag) {
        (Some(positional), Some(flag)) if positional != flag => {
            bail!(
                "PR number mismatch: positional target resolves to #{} but --pr={}",
                positional,
                flag
            )
        }
        (Some(pr), _) | (_, Some(pr)) => Ok(pr),
        (None, None) => {
            bail!("PR number is required. Provide a PR URL, owner/repo#number, or --pr.")
        }
    }
}

fn resolve_trigger_args(args: &TriggerArgs) -> Result<(Option<String>, u64)> {
    let positional = args
        .target
        .as_deref()
        .map(parse_trigger_target)
        .transpose()?;
    let positional_repo = positional.as_ref().map(|target| target.repo.clone());
    let positional_pr = positional.map(|target| target.pr);

    let repo =
        merge_trigger_repo_arg(positional_repo, args.repo.clone(), args.repo_path.is_some())?;
    let pr = merge_trigger_pr_arg(positional_pr, args.pr)?;

    Ok((repo, pr))
}

fn trigger_repo_candidates(repos_root: &Path, repo_name: &str) -> Vec<PathBuf> {
    let mut parts = repo_name.split('/');
    let Some(owner) = parts.next() else {
        return Vec::new();
    };
    let Some(name) = parts.next() else {
        return Vec::new();
    };

    let mut candidates = Vec::new();
    push_unique_trigger_candidate(&mut candidates, repos_root.join(name));
    push_unique_trigger_candidate(&mut candidates, repos_root.join(owner).join(name));
    push_unique_trigger_candidate(&mut candidates, repos_root.join(format!("{owner}-{name}")));
    push_unique_trigger_candidate(&mut candidates, repos_root.to_path_buf());

    if let Ok(entries) = std::fs::read_dir(repos_root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                push_unique_trigger_candidate(&mut candidates, path.join(name));
                push_unique_trigger_candidate(&mut candidates, path.join(owner).join(name));
                push_unique_trigger_candidate(
                    &mut candidates,
                    path.join(format!("{owner}-{name}")),
                );
            }
        }
    }

    candidates
}

fn push_unique_trigger_candidate(candidates: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !candidates.contains(&candidate) {
        candidates.push(candidate);
    }
}

fn resolve_trigger_repo(
    repos_root: &Path,
    args: &TriggerArgs,
    expected_repo: Option<&str>,
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

        if let Some(expected_repo) = expected_repo {
            if expected_repo != repo_name {
                bail!(
                    "Repo mismatch: target repo is {} but --repo-path resolves to {}",
                    expected_repo,
                    repo_name
                );
            }
        }

        return Ok((repo_path.clone(), repo_name));
    }

    let repo_name =
        expected_repo.context("Either a target repo or --repo-path must be provided")?;
    for candidate in trigger_repo_candidates(repos_root, repo_name) {
        if !candidate.is_dir() || !candidate.join(".git").exists() {
            continue;
        }
        if gh::repo_name_with_owner(&candidate).as_deref() == Some(repo_name) {
            return Ok((candidate, repo_name.to_string()));
        }
    }

    bail!(
        "Repo {} is not cloned under {}. Clone it there or pass --repo-path.",
        repo_name,
        repos_root.display()
    );
}

fn run_trigger_command(
    cfg: &mut config::Config,
    root_override: Option<PathBuf>,
    trigger_args: TriggerArgs,
) -> Result<()> {
    let repos_root = resolve_repos_root(cfg, root_override)?;
    let (expected_repo, pr_number) = resolve_trigger_args(&trigger_args)?;
    let (repo_path, repo_name) =
        resolve_trigger_repo(&repos_root, &trigger_args, expected_repo.as_deref())?;

    println!("Triggering review for {}#{}...", repo_name, pr_number);

    let pr = gh::fetch_pr_for_review(&repo_path, &repo_name, pr_number)?;
    gh::validate_ai_launch_config(&cfg.ai)?;

    let worktree_path = gh::create_pr_worktree(&pr, &repos_root)
        .with_context(|| format!("Failed to create worktree for {}#{}", repo_name, pr_number))?;
    gh::launch_ai(&worktree_path, &pr, &cfg.ai)
        .with_context(|| format!("Failed to launch review for {}#{}", repo_name, pr_number))?;

    println!(
        "Triggered review for {}#{} (worktree: {})",
        repo_name,
        pr_number,
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
            run_trigger_command(&mut cfg, args.root, trigger_args)
        }
        None => {
            let username = gh::get_current_user()?;
            println!("Authenticated as: {}\n", username);
            let repos_root = resolve_tui_repos_root(&cfg, args.root)?;
            run_tui(
                cfg.ai.clone(),
                repos_root,
                username,
                args.drafts,
                args.my,
                cfg.exclude_users.clone(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_trigger_target, resolve_trigger_args, Args, Commands};
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

    #[test]
    fn trigger_parses_with_url_target() {
        let parsed = Args::try_parse_from([
            "reviewer",
            "trigger",
            "https://github.com/nvidia-lpu/cyborg/pull/199",
        ]);
        assert!(parsed.is_ok());
    }

    #[test]
    fn parse_trigger_target_accepts_github_url() {
        let target = parse_trigger_target("https://github.com/nvidia-lpu/cyborg/pull/199").unwrap();
        assert_eq!(target.repo, "nvidia-lpu/cyborg");
        assert_eq!(target.pr, 199);
    }

    #[test]
    fn parse_trigger_target_accepts_repo_hash_number() {
        let target = parse_trigger_target("nvidia-lpu/cyborg#199").unwrap();
        assert_eq!(target.repo, "nvidia-lpu/cyborg");
        assert_eq!(target.pr, 199);
    }

    #[test]
    fn trigger_target_flags_must_not_conflict() {
        let parsed = Args::try_parse_from([
            "reviewer",
            "trigger",
            "https://github.com/nvidia-lpu/cyborg/pull/199",
            "--repo",
            "nvidia-lpu/other",
        ])
        .unwrap();

        let Some(Commands::Trigger(trigger_args)) = parsed.command else {
            panic!("expected trigger command");
        };

        let err = resolve_trigger_args(&trigger_args).expect_err("expected mismatch");
        assert!(format!("{err:#}").contains("Repo mismatch"));
    }
}
