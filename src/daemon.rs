use crate::config::{self, AiConfig, Config};
use crate::gh::{self, PullRequest};
use crate::repos;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{Frame, Terminal};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
struct RepoDescriptor {
    path: PathBuf,
    name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TriggerStatus {
    Seeded,
    Success,
    Failed,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ReviewedPrRecord {
    pub repo: String,
    pub pr_number: u64,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub latest_updated_at: DateTime<Utc>,
    pub triggered_at: Option<DateTime<Utc>>,
    pub trigger_status: TriggerStatus,
    pub last_error: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DaemonState {
    #[serde(default)]
    pub prs: HashMap<String, ReviewedPrRecord>,
    #[serde(default)]
    pub last_poll_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
pub struct PollSummary {
    pub monitored_repos: usize,
    pub open_prs: usize,
    pub new_prs: usize,
    pub triggered: usize,
    pub failed: usize,
}

#[derive(Debug)]
pub struct DaemonStatus {
    pub state_path: PathBuf,
    pub initialized: bool,
    pub poll_interval_sec: u64,
    pub include_drafts: bool,
    pub excluded_repos: Vec<String>,
    pub reviewed_count: usize,
    pub seeded_count: usize,
    pub success_count: usize,
    pub failed_count: usize,
    pub last_poll_at: Option<DateTime<Utc>>,
}

pub fn state_path() -> PathBuf {
    config::config_dir().join("daemon_state.json")
}

fn load_state() -> DaemonState {
    let path = state_path();
    if !path.exists() {
        return DaemonState::default();
    }
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(state: &DaemonState) -> Result<()> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(path, json)?;
    Ok(())
}

fn pr_key(repo: &str, pr_number: u64) -> String {
    format!("{repo}#{pr_number}")
}

fn discover_repos(repos_root: &Path, exclude_dirs: &[String]) -> Vec<RepoDescriptor> {
    repos::scan_unique_repos(repos_root, 3, exclude_dirs)
        .unique_repos
        .into_iter()
        .filter_map(|repo| {
            repo.name_with_owner.map(|name| RepoDescriptor {
                path: repo.path,
                name,
            })
        })
        .collect()
}

fn monitored_repo_set(exclude_repos: &[String]) -> HashSet<String> {
    exclude_repos.iter().cloned().collect()
}

fn normalize_repo_names(mut repos: Vec<String>) -> Vec<String> {
    repos.sort();
    repos.dedup();
    repos
}

fn collect_open_prs(
    repos: &[RepoDescriptor],
    excluded_repos: &HashSet<String>,
    username: &str,
    include_drafts: bool,
) -> Vec<PullRequest> {
    repos
        .par_iter()
        .filter(|repo| !excluded_repos.contains(&repo.name))
        .flat_map(|repo| gh::fetch_prs_for_repo(&repo.path, username, include_drafts))
        .collect()
}

fn build_seed_record(pr: &PullRequest, now: DateTime<Utc>) -> ReviewedPrRecord {
    ReviewedPrRecord {
        repo: pr.repo_name.clone(),
        pr_number: pr.number,
        first_seen_at: now,
        last_seen_at: now,
        latest_updated_at: pr.updated_at,
        triggered_at: None,
        trigger_status: TriggerStatus::Seeded,
        last_error: None,
    }
}

fn trigger_review(pr: &PullRequest, repos_root: &Path, ai: &AiConfig) -> Result<()> {
    let worktree_path = gh::create_pr_worktree(pr, repos_root).with_context(|| {
        format!(
            "Failed to create worktree for {}#{}",
            pr.repo_name, pr.number
        )
    })?;
    gh::launch_ai(&worktree_path, pr, ai).with_context(|| {
        format!(
            "Failed to launch AI review for {}#{}",
            pr.repo_name, pr.number
        )
    })?;
    Ok(())
}

fn seed_existing_open_prs(
    state: &mut DaemonState,
    repos: &[RepoDescriptor],
    cfg: &Config,
    username: &str,
) -> usize {
    let excluded_repos = monitored_repo_set(&cfg.daemon.exclude_repos);
    let prs = collect_open_prs(repos, &excluded_repos, username, cfg.daemon.include_drafts);
    let now = Utc::now();
    let mut seeded = 0usize;

    for pr in prs {
        let key = pr_key(&pr.repo_name, pr.number);
        if let Some(existing) = state.prs.get_mut(&key) {
            existing.last_seen_at = now;
            existing.latest_updated_at = pr.updated_at;
            continue;
        }
        state.prs.insert(key, build_seed_record(&pr, now));
        seeded += 1;
    }

    state.last_poll_at = Some(now);
    seeded
}

pub fn init(cfg: &mut Config, repos_root: &Path, username: &str) -> Result<()> {
    let repos = discover_repos(repos_root, &cfg.exclude);
    if repos.is_empty() {
        cfg.daemon.initialized = true;
        config::save_config(cfg)?;
        println!("No repositories discovered under {}.", repos_root.display());
        return Ok(());
    }

    let excluded = run_repo_selector(&repos, &cfg.daemon.exclude_repos)?;
    cfg.daemon.exclude_repos = normalize_repo_names(excluded);
    cfg.daemon.initialized = true;
    config::save_config(cfg)?;

    let mut state = load_state();
    let seeded = seed_existing_open_prs(&mut state, &repos, cfg, username);
    save_state(&state)?;

    println!(
        "Daemon initialized. Monitoring {} repos ({} excluded). Seeded {} existing PRs as already seen.",
        repos.len().saturating_sub(cfg.daemon.exclude_repos.len()),
        cfg.daemon.exclude_repos.len(),
        seeded
    );

    Ok(())
}

pub fn poll_once(cfg: &Config, repos_root: &Path, username: &str) -> Result<PollSummary> {
    let repos = discover_repos(repos_root, &cfg.exclude);
    let excluded_repos = monitored_repo_set(&cfg.daemon.exclude_repos);
    let monitored_repos = repos
        .iter()
        .filter(|repo| !excluded_repos.contains(&repo.name))
        .count();
    let open_prs = collect_open_prs(&repos, &excluded_repos, username, cfg.daemon.include_drafts);
    let open_pr_count = open_prs.len();

    let now = Utc::now();
    let mut state = load_state();
    let mut new_prs = 0usize;
    let mut triggered = 0usize;
    let mut failed = 0usize;

    for pr in open_prs {
        let key = pr_key(&pr.repo_name, pr.number);
        if let Some(existing) = state.prs.get_mut(&key) {
            existing.last_seen_at = now;
            existing.latest_updated_at = pr.updated_at;
            continue;
        }

        new_prs += 1;
        println!(
            "New PR detected: {}#{} - {}",
            pr.repo_name, pr.number, pr.title
        );

        let mut record = build_seed_record(&pr, now);
        match trigger_review(&pr, repos_root, &cfg.ai) {
            Ok(()) => {
                record.triggered_at = Some(Utc::now());
                record.trigger_status = TriggerStatus::Success;
                triggered += 1;
                println!("Triggered review for {}#{}", pr.repo_name, pr.number);
            }
            Err(err) => {
                record.trigger_status = TriggerStatus::Failed;
                record.last_error = Some(err.to_string());
                failed += 1;
                eprintln!(
                    "Failed to trigger review for {}#{}: {}",
                    pr.repo_name, pr.number, err
                );
            }
        }

        state.prs.insert(key, record);
    }

    state.last_poll_at = Some(now);
    save_state(&state)?;

    Ok(PollSummary {
        monitored_repos,
        open_prs: open_pr_count,
        new_prs,
        triggered,
        failed,
    })
}

pub fn run(
    cfg: &Config,
    repos_root: &Path,
    username: &str,
    poll_interval_override: Option<u64>,
    once: bool,
) -> Result<()> {
    if !cfg.daemon.initialized {
        return Err(anyhow!(
            "Daemon is not initialized. Run `reviewer daemon init` first."
        ));
    }

    let poll_interval_sec = poll_interval_override
        .unwrap_or(cfg.daemon.poll_interval_sec)
        .max(10);
    println!(
        "Daemon running. Poll interval: {}s. Include drafts: {}.",
        poll_interval_sec, cfg.daemon.include_drafts
    );

    loop {
        let summary = poll_once(cfg, repos_root, username)?;
        println!(
            "Poll complete: {} repos, {} open PRs, {} new, {} triggered, {} failed.",
            summary.monitored_repos,
            summary.open_prs,
            summary.new_prs,
            summary.triggered,
            summary.failed
        );

        if once {
            break;
        }
        thread::sleep(Duration::from_secs(poll_interval_sec));
    }

    Ok(())
}

pub fn status(cfg: &Config) -> DaemonStatus {
    let state = load_state();
    let mut seeded_count = 0usize;
    let mut success_count = 0usize;
    let mut failed_count = 0usize;
    for record in state.prs.values() {
        match record.trigger_status {
            TriggerStatus::Seeded => seeded_count += 1,
            TriggerStatus::Success => success_count += 1,
            TriggerStatus::Failed => failed_count += 1,
        }
    }

    let excluded_repos = normalize_repo_names(cfg.daemon.exclude_repos.clone());

    DaemonStatus {
        state_path: state_path(),
        initialized: cfg.daemon.initialized,
        poll_interval_sec: cfg.daemon.poll_interval_sec,
        include_drafts: cfg.daemon.include_drafts,
        excluded_repos,
        reviewed_count: state.prs.len(),
        seeded_count,
        success_count,
        failed_count,
        last_poll_at: state.last_poll_at,
    }
}

struct RepoSelector {
    repos: Vec<String>,
    included: Vec<bool>,
    list_state: ListState,
}

impl RepoSelector {
    fn new(repos: &[RepoDescriptor], pre_excluded: &[String]) -> Self {
        let excluded: HashSet<String> = pre_excluded.iter().cloned().collect();
        let names: Vec<String> = repos.iter().map(|repo| repo.name.clone()).collect();
        let included: Vec<bool> = names.iter().map(|name| !excluded.contains(name)).collect();

        let mut list_state = ListState::default();
        if !names.is_empty() {
            list_state.select(Some(0));
        }

        Self {
            repos: names,
            included,
            list_state,
        }
    }

    fn selected(&self) -> Option<usize> {
        self.list_state.selected()
    }

    fn next(&mut self) {
        if self.repos.is_empty() {
            return;
        }
        let idx = self.selected().unwrap_or(0);
        let next = if idx + 1 >= self.repos.len() {
            0
        } else {
            idx + 1
        };
        self.list_state.select(Some(next));
    }

    fn previous(&mut self) {
        if self.repos.is_empty() {
            return;
        }
        let idx = self.selected().unwrap_or(0);
        let prev = if idx == 0 {
            self.repos.len() - 1
        } else {
            idx.saturating_sub(1)
        };
        self.list_state.select(Some(prev));
    }

    fn toggle_selected(&mut self) {
        if let Some(idx) = self.selected() {
            if let Some(value) = self.included.get_mut(idx) {
                *value = !*value;
            }
        }
    }

    fn include_all(&mut self) {
        self.included.fill(true);
    }

    fn exclude_all(&mut self) {
        self.included.fill(false);
    }

    fn excluded_repos(self) -> Vec<String> {
        self.repos
            .into_iter()
            .zip(self.included)
            .filter_map(|(repo, included)| if included { None } else { Some(repo) })
            .collect()
    }
}

fn draw_repo_selector(frame: &mut Frame, app: &mut RepoSelector) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(frame.area());

    let items: Vec<ListItem> = app
        .repos
        .iter()
        .zip(app.included.iter())
        .map(|(repo, included)| {
            let marker = if *included { "[x]" } else { "[ ]" };
            ListItem::new(Line::from(format!("{marker} {repo}")))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Select Repositories to Monitor ")
                .borders(Borders::ALL),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_stateful_widget(list, chunks[0], &mut app.list_state);

    let help = Paragraph::new(
        "j/k or arrows: move | space: toggle | a: include all | x: exclude all | Enter: save | q: cancel",
    )
    .block(Block::default().borders(Borders::ALL).title(" Controls "));
    frame.render_widget(help, chunks[1]);
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode().context("Failed to disable raw mode")?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn run_repo_selector(repos: &[RepoDescriptor], pre_excluded: &[String]) -> Result<Vec<String>> {
    let mut app = RepoSelector::new(repos, pre_excluded);
    let mut terminal = setup_terminal()?;

    let result = (|| -> Result<Vec<String>> {
        loop {
            terminal.draw(|frame| draw_repo_selector(frame, &mut app))?;

            if !event::poll(Duration::from_millis(250))? {
                continue;
            }

            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => app.next(),
                    KeyCode::Char('k') | KeyCode::Up => app.previous(),
                    KeyCode::Char(' ') => app.toggle_selected(),
                    KeyCode::Char('a') => app.include_all(),
                    KeyCode::Char('x') => app.exclude_all(),
                    KeyCode::Enter => break Ok(app.excluded_repos()),
                    KeyCode::Esc | KeyCode::Char('q') => {
                        break Err(anyhow!("Daemon initialization cancelled"))
                    }
                    _ => {}
                }
            }
        }
    })();

    restore_terminal(&mut terminal)?;
    result
}
