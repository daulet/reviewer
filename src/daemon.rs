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
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
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
    pub repo_subpath_filters: Vec<RepoSubpathFilterStatus>,
    pub reviewed_count: usize,
    pub seeded_count: usize,
    pub success_count: usize,
    pub failed_count: usize,
    pub last_poll_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct RepoSubpathFilterStatus {
    pub repo: String,
    pub subpaths: Vec<String>,
}

type RepoSubpathFilterMap = HashMap<String, Vec<String>>;
type RepoSelectionConfig = (Vec<String>, RepoSubpathFilterMap);

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

fn normalize_subpath(path: &str) -> Option<String> {
    let normalized = path.trim().trim_matches('/').trim();
    if normalized.is_empty() {
        return None;
    }
    Some(normalized.to_string())
}

fn normalize_subpaths(paths: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = paths
        .iter()
        .filter_map(|path| normalize_subpath(path))
        .collect();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn normalize_repo_subpath_filters(
    repo_subpath_filters: &RepoSubpathFilterMap,
) -> RepoSubpathFilterMap {
    let mut normalized = HashMap::new();
    for (repo, subpaths) in repo_subpath_filters {
        let repo_name = repo.trim();
        if repo_name.is_empty() {
            continue;
        }
        normalized.insert(repo_name.to_string(), normalize_subpaths(subpaths));
    }
    normalized
}

fn normalize_repo_subpath_filter_status(
    repo_subpath_filters: &RepoSubpathFilterMap,
) -> Vec<RepoSubpathFilterStatus> {
    let mut normalized: Vec<RepoSubpathFilterStatus> =
        normalize_repo_subpath_filters(repo_subpath_filters)
            .into_iter()
            .filter(|(_, subpaths)| !subpaths.is_empty())
            .map(|(repo, subpaths)| RepoSubpathFilterStatus { repo, subpaths })
            .collect();
    normalized.sort_by(|a, b| a.repo.cmp(&b.repo));
    normalized
}

fn path_matches_subpath(path: &str, subpath: &str) -> bool {
    let normalized_path = path.trim_start_matches('/');
    normalized_path == subpath
        || normalized_path
            .strip_prefix(subpath)
            .map(|rest| rest.starts_with('/'))
            .unwrap_or(false)
}

fn pr_touches_any_subpath(changed_files: &[String], subpaths: &[String]) -> bool {
    changed_files.iter().any(|path| {
        subpaths
            .iter()
            .any(|subpath| path_matches_subpath(path, subpath))
    })
}

fn apply_repo_subpath_filter(
    repo: &RepoDescriptor,
    prs: Vec<PullRequest>,
    repo_subpath_filters: &RepoSubpathFilterMap,
) -> Vec<PullRequest> {
    let subpaths = match repo_subpath_filters.get(&repo.name) {
        Some(subpaths) if !subpaths.is_empty() => subpaths,
        _ => return prs,
    };

    prs.into_iter()
        .filter(|pr| match gh::get_pr_changed_files(pr) {
            Ok(changed_files) => pr_touches_any_subpath(&changed_files, subpaths),
            Err(err) => {
                eprintln!(
                    "Failed to evaluate daemon subpath filter for {}#{}: {}. Triggering review anyway.",
                    pr.repo_name, pr.number, err
                );
                true
            }
        })
        .collect()
}

fn collect_open_prs(
    repos: &[RepoDescriptor],
    excluded_repos: &HashSet<String>,
    repo_subpath_filters: &RepoSubpathFilterMap,
    username: &str,
    include_drafts: bool,
) -> Vec<PullRequest> {
    repos
        .par_iter()
        .filter(|repo| !excluded_repos.contains(&repo.name))
        .flat_map(|repo| {
            let prs = gh::fetch_prs_for_repo(&repo.path, username, include_drafts);
            apply_repo_subpath_filter(repo, prs, repo_subpath_filters)
        })
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
    let repo_subpath_filters = normalize_repo_subpath_filters(&cfg.daemon.repo_subpath_filters);
    let prs = collect_open_prs(
        repos,
        &excluded_repos,
        &repo_subpath_filters,
        username,
        cfg.daemon.include_drafts,
    );
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

    let (excluded, repo_subpath_filters) = run_repo_selector(
        &repos,
        &cfg.daemon.exclude_repos,
        &cfg.daemon.repo_subpath_filters,
    )?;
    cfg.daemon.exclude_repos = normalize_repo_names(excluded);
    cfg.daemon.repo_subpath_filters = normalize_repo_subpath_filters(&repo_subpath_filters);
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
    let repo_subpath_filters = normalize_repo_subpath_filters(&cfg.daemon.repo_subpath_filters);
    let monitored_repos = repos
        .iter()
        .filter(|repo| !excluded_repos.contains(&repo.name))
        .count();
    let open_prs = collect_open_prs(
        &repos,
        &excluded_repos,
        &repo_subpath_filters,
        username,
        cfg.daemon.include_drafts,
    );
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
    let subpath_filter_count =
        normalize_repo_subpath_filter_status(&cfg.daemon.repo_subpath_filters).len();
    println!(
        "Daemon running. Poll interval: {}s. Include drafts: {}. Repo subpath filters: {}.",
        poll_interval_sec, cfg.daemon.include_drafts, subpath_filter_count
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
    let repo_subpath_filters =
        normalize_repo_subpath_filter_status(&cfg.daemon.repo_subpath_filters);

    DaemonStatus {
        state_path: state_path(),
        initialized: cfg.daemon.initialized,
        poll_interval_sec: cfg.daemon.poll_interval_sec,
        include_drafts: cfg.daemon.include_drafts,
        excluded_repos,
        repo_subpath_filters,
        reviewed_count: state.prs.len(),
        seeded_count,
        success_count,
        failed_count,
        last_poll_at: state.last_poll_at,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepoSelectorMode {
    Browse,
    EditSubpaths,
}

#[derive(Debug, Clone)]
struct RepoTreeNode {
    name: String,
    rel_path: String,
    children: Vec<RepoTreeNode>,
    has_children: bool,
    expanded: bool,
    loaded: bool,
}

#[derive(Debug, Clone)]
struct VisibleRepoTreeNode {
    index_path: Vec<usize>,
    depth: usize,
    name: String,
    rel_path: String,
    has_children: bool,
    expanded: bool,
}

#[derive(Debug, Clone)]
struct SubpathTreeEditor {
    repo_root: PathBuf,
    nodes: Vec<RepoTreeNode>,
    selected_paths: HashSet<String>,
    cursor: usize,
}

fn should_skip_repo_dir(name: &str) -> bool {
    name == ".git"
}

fn has_child_directories(path: &Path) -> bool {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return false,
    };

    for entry in entries.flatten() {
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if !file_type.is_dir() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip_repo_dir(&name) {
            continue;
        }
        return true;
    }

    false
}

fn load_directory_nodes(repo_root: &Path, rel_path: &str) -> Vec<RepoTreeNode> {
    let base_path = if rel_path.is_empty() {
        repo_root.to_path_buf()
    } else {
        repo_root.join(rel_path)
    };

    let entries = match std::fs::read_dir(base_path) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut nodes = Vec::new();
    for entry in entries.flatten() {
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if !file_type.is_dir() {
            continue;
        }

        let name = entry.file_name().to_string_lossy().to_string();
        if should_skip_repo_dir(&name) {
            continue;
        }

        let child_rel_path = if rel_path.is_empty() {
            name.clone()
        } else {
            format!("{rel_path}/{name}")
        };

        nodes.push(RepoTreeNode {
            name,
            rel_path: child_rel_path.clone(),
            children: Vec::new(),
            has_children: has_child_directories(&repo_root.join(&child_rel_path)),
            expanded: false,
            loaded: false,
        });
    }

    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    nodes
}

fn collect_visible_nodes(
    nodes: &[RepoTreeNode],
    depth: usize,
    index_prefix: &mut Vec<usize>,
    visible: &mut Vec<VisibleRepoTreeNode>,
) {
    for (idx, node) in nodes.iter().enumerate() {
        index_prefix.push(idx);
        visible.push(VisibleRepoTreeNode {
            index_path: index_prefix.clone(),
            depth,
            name: node.name.clone(),
            rel_path: node.rel_path.clone(),
            has_children: node.has_children,
            expanded: node.expanded,
        });

        if node.expanded {
            collect_visible_nodes(&node.children, depth + 1, index_prefix, visible);
        }
        index_prefix.pop();
    }
}

fn get_tree_node_mut<'a>(
    nodes: &'a mut [RepoTreeNode],
    index_path: &[usize],
) -> Option<&'a mut RepoTreeNode> {
    let (first_idx, rest) = index_path.split_first()?;
    let node = nodes.get_mut(*first_idx)?;
    if rest.is_empty() {
        Some(node)
    } else {
        get_tree_node_mut(&mut node.children, rest)
    }
}

impl SubpathTreeEditor {
    fn new(repo_root: PathBuf, preselected_paths: &[String]) -> Self {
        let selected_paths: HashSet<String> =
            normalize_subpaths(preselected_paths).into_iter().collect();

        Self {
            nodes: load_directory_nodes(&repo_root, ""),
            repo_root,
            selected_paths,
            cursor: 0,
        }
    }

    fn visible_nodes(&self) -> Vec<VisibleRepoTreeNode> {
        let mut visible = Vec::new();
        collect_visible_nodes(&self.nodes, 0, &mut Vec::new(), &mut visible);
        visible
    }

    fn next(&mut self) {
        let visible_len = self.visible_nodes().len();
        if visible_len == 0 {
            return;
        }
        self.cursor = (self.cursor + 1).min(visible_len.saturating_sub(1));
    }

    fn previous(&mut self) {
        let visible_len = self.visible_nodes().len();
        if visible_len == 0 {
            return;
        }
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    fn toggle_selected(&mut self) {
        let current = match self.visible_nodes().get(self.cursor).cloned() {
            Some(current) => current,
            None => return,
        };
        if self.selected_paths.contains(&current.rel_path) {
            self.selected_paths.remove(&current.rel_path);
        } else {
            self.selected_paths.insert(current.rel_path);
        }
    }

    fn toggle_expand_selected(&mut self) {
        let current = match self.visible_nodes().get(self.cursor).cloned() {
            Some(current) => current,
            None => return,
        };
        let node = match get_tree_node_mut(&mut self.nodes, &current.index_path) {
            Some(node) => node,
            None => return,
        };

        if !node.has_children {
            return;
        }

        if !node.loaded {
            node.children = load_directory_nodes(&self.repo_root, &node.rel_path);
            node.loaded = true;
        }
        node.expanded = !node.expanded;

        let visible_len = self.visible_nodes().len();
        if visible_len == 0 {
            self.cursor = 0;
        } else if self.cursor >= visible_len {
            self.cursor = visible_len - 1;
        }
    }

    fn is_selected(&self, rel_path: &str) -> bool {
        self.selected_paths.contains(rel_path)
    }

    fn selected_count(&self) -> usize {
        self.selected_paths.len()
    }

    fn into_selected_paths(self) -> Vec<String> {
        let mut paths: Vec<String> = self.selected_paths.into_iter().collect();
        paths.sort();
        paths
    }
}

struct RepoSelector {
    repos: Vec<String>,
    repo_paths: Vec<PathBuf>,
    included: Vec<bool>,
    subpath_filters: Vec<Vec<String>>,
    mode: RepoSelectorMode,
    subpath_editor: Option<SubpathTreeEditor>,
    list_state: ListState,
}

impl RepoSelector {
    fn new(
        repos: &[RepoDescriptor],
        pre_excluded: &[String],
        pre_subpath_filters: &RepoSubpathFilterMap,
    ) -> Self {
        let excluded: HashSet<String> = pre_excluded.iter().cloned().collect();
        let normalized_pre_filters = normalize_repo_subpath_filters(pre_subpath_filters);
        let names: Vec<String> = repos.iter().map(|repo| repo.name.clone()).collect();
        let repo_paths: Vec<PathBuf> = repos.iter().map(|repo| repo.path.clone()).collect();
        let included: Vec<bool> = names.iter().map(|name| !excluded.contains(name)).collect();
        let subpath_filters: Vec<Vec<String>> = names
            .iter()
            .map(|name| {
                normalized_pre_filters
                    .get(name)
                    .cloned()
                    .unwrap_or_default()
            })
            .collect();

        let mut list_state = ListState::default();
        if !names.is_empty() {
            list_state.select(Some(0));
        }

        Self {
            repos: names,
            repo_paths,
            included,
            subpath_filters,
            mode: RepoSelectorMode::Browse,
            subpath_editor: None,
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

    fn selected_repo_name(&self) -> Option<&str> {
        self.selected()
            .and_then(|idx| self.repos.get(idx))
            .map(|name| name.as_str())
    }

    fn selected_subpaths(&self) -> Option<&[String]> {
        self.selected()
            .and_then(|idx| self.subpath_filters.get(idx))
            .map(|paths| paths.as_slice())
    }

    fn selected_repo_details(&self) -> String {
        let Some(repo) = self.selected_repo_name() else {
            return "Selected: none".to_string();
        };
        let Some(subpaths) = self.selected_subpaths() else {
            return format!("Selected: {repo} (all PRs)");
        };
        if subpaths.is_empty() {
            format!("Selected: {repo} (all PRs)")
        } else {
            format!("Selected: {repo} (paths: {})", subpaths.join(", "))
        }
    }

    fn is_editing_subpaths(&self) -> bool {
        self.mode == RepoSelectorMode::EditSubpaths
    }

    fn start_edit_subpaths(&mut self) {
        let Some(idx) = self.selected() else {
            return;
        };
        let Some(repo_path) = self.repo_paths.get(idx).cloned() else {
            return;
        };
        let preselected = self.subpath_filters.get(idx).cloned().unwrap_or_default();
        self.subpath_editor = Some(SubpathTreeEditor::new(repo_path, &preselected));
        self.mode = RepoSelectorMode::EditSubpaths;
    }

    fn subpath_editor_next(&mut self) {
        if let Some(editor) = self.subpath_editor.as_mut() {
            editor.next();
        }
    }

    fn subpath_editor_previous(&mut self) {
        if let Some(editor) = self.subpath_editor.as_mut() {
            editor.previous();
        }
    }

    fn subpath_editor_toggle_selected(&mut self) {
        if let Some(editor) = self.subpath_editor.as_mut() {
            editor.toggle_selected();
        }
    }

    fn subpath_editor_toggle_expand_selected(&mut self) {
        if let Some(editor) = self.subpath_editor.as_mut() {
            editor.toggle_expand_selected();
        }
    }

    fn save_subpaths_input(&mut self) {
        if let Some(idx) = self.selected() {
            if let Some(editor) = self.subpath_editor.take() {
                self.subpath_filters[idx] = editor.into_selected_paths();
            }
        }
        self.mode = RepoSelectorMode::Browse;
    }

    fn cancel_subpaths_input(&mut self) {
        self.subpath_editor = None;
        self.mode = RepoSelectorMode::Browse;
    }

    fn into_config(self) -> RepoSelectionConfig {
        let mut excluded_repos = Vec::new();
        let mut repo_subpath_filters = HashMap::new();

        let Self {
            repos,
            included,
            subpath_filters,
            ..
        } = self;

        for ((repo, included), subpaths) in repos
            .into_iter()
            .zip(included.into_iter())
            .zip(subpath_filters.into_iter())
        {
            if !included {
                excluded_repos.push(repo.clone());
            }
            if !subpaths.is_empty() {
                repo_subpath_filters.insert(repo, subpaths);
            }
        }

        (excluded_repos, repo_subpath_filters)
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn draw_subpath_popup(frame: &mut Frame, app: &RepoSelector) {
    let popup_area = centered_rect(85, 45, frame.area());
    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(" Edit PR Path Filters ")
        .borders(Borders::ALL);
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let editor = match app.subpath_editor.as_ref() {
        Some(editor) => editor,
        None => return,
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(4),
            Constraint::Length(2),
        ])
        .split(inner);

    let repo_name = app.selected_repo_name().unwrap_or("unknown");
    let header = Paragraph::new(vec![
        Line::from(format!("Repo: {repo_name}")),
        Line::from("Use Enter to expand/collapse, Space to mark path."),
        Line::from("Press s to save selection, Esc to cancel."),
    ])
    .wrap(Wrap { trim: true });
    frame.render_widget(header, chunks[0]);

    let visible_nodes = editor.visible_nodes();
    if visible_nodes.is_empty() {
        let empty = Paragraph::new("No subdirectories found in this repository.")
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Directories "),
            );
        frame.render_widget(empty, chunks[1]);
    } else {
        let items: Vec<ListItem> = visible_nodes
            .iter()
            .map(|node| {
                let indent = "  ".repeat(node.depth);
                let expand_marker = if node.has_children {
                    if node.expanded {
                        "-"
                    } else {
                        "+"
                    }
                } else {
                    " "
                };
                let selected_marker = if editor.is_selected(&node.rel_path) {
                    "[x]"
                } else {
                    "[ ]"
                };
                ListItem::new(Line::from(format!(
                    "{indent}{expand_marker} {selected_marker} {}",
                    node.name
                )))
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Directories "),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            );
        let mut list_state = ListState::default();
        list_state.select(Some(editor.cursor.min(visible_nodes.len() - 1)));
        frame.render_stateful_widget(list, chunks[1], &mut list_state);
    }

    let footer = Paragraph::new(format!("Selected paths: {}", editor.selected_count()))
        .wrap(Wrap { trim: true });
    frame.render_widget(footer, chunks[2]);
}

fn draw_repo_selector(frame: &mut Frame, app: &mut RepoSelector) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(4)])
        .split(frame.area());

    let items: Vec<ListItem> = app
        .repos
        .iter()
        .zip(app.included.iter())
        .zip(app.subpath_filters.iter())
        .map(|((repo, included), subpaths)| {
            let marker = if *included { "[x]" } else { "[ ]" };
            let subpath_marker = if subpaths.is_empty() {
                "all".to_string()
            } else {
                format!("paths:{}", subpaths.len())
            };
            ListItem::new(Line::from(format!("{marker} {repo} [{subpath_marker}]")))
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

    let help_lines = if app.is_editing_subpaths() {
        vec![
            Line::from("Editing subpath filters in popup"),
            Line::from("j/k: move | Enter: expand/collapse | Space: mark | s: save | Esc: cancel"),
        ]
    } else {
        vec![
            Line::from(
                "j/k or arrows: move | space: toggle | f: edit paths | a: include all | x: exclude all | Enter: save | q: cancel",
            ),
            Line::from(app.selected_repo_details()),
        ]
    };
    let help = Paragraph::new(help_lines)
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title(" Controls "));
    frame.render_widget(help, chunks[1]);

    if app.is_editing_subpaths() {
        draw_subpath_popup(frame, app);
    }
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

fn run_repo_selector(
    repos: &[RepoDescriptor],
    pre_excluded: &[String],
    pre_subpath_filters: &RepoSubpathFilterMap,
) -> Result<RepoSelectionConfig> {
    let mut app = RepoSelector::new(repos, pre_excluded, pre_subpath_filters);
    let mut terminal = setup_terminal()?;

    let result = (|| -> Result<RepoSelectionConfig> {
        loop {
            terminal.draw(|frame| draw_repo_selector(frame, &mut app))?;

            if !event::poll(Duration::from_millis(250))? {
                continue;
            }

            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if app.is_editing_subpaths() {
                    match key.code {
                        KeyCode::Char('j') | KeyCode::Down => app.subpath_editor_next(),
                        KeyCode::Char('k') | KeyCode::Up => app.subpath_editor_previous(),
                        KeyCode::Char(' ') => app.subpath_editor_toggle_selected(),
                        KeyCode::Enter => app.subpath_editor_toggle_expand_selected(),
                        KeyCode::Char('s') => app.save_subpaths_input(),
                        KeyCode::Esc => app.cancel_subpaths_input(),
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => app.next(),
                    KeyCode::Char('k') | KeyCode::Up => app.previous(),
                    KeyCode::Char(' ') => app.toggle_selected(),
                    KeyCode::Char('a') => app.include_all(),
                    KeyCode::Char('x') => app.exclude_all(),
                    KeyCode::Char('f') => app.start_edit_subpaths(),
                    KeyCode::Enter => break Ok(app.into_config()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_subpaths_trims_and_dedups() {
        let paths = vec![
            " src ".to_string(),
            "/src/".to_string(),
            "services/api".to_string(),
            "".to_string(),
            "   ".to_string(),
        ];

        assert_eq!(
            normalize_subpaths(&paths),
            vec!["services/api".to_string(), "src".to_string()]
        );
    }

    #[test]
    fn path_matches_subpath_enforces_path_boundaries() {
        assert!(path_matches_subpath("src/main.rs", "src"));
        assert!(path_matches_subpath("src", "src"));
        assert!(!path_matches_subpath("src2/main.rs", "src"));
        assert!(!path_matches_subpath("nested/src/main.rs", "src"));
    }

    #[test]
    fn pr_touches_any_subpath_matches_any_changed_file() {
        let changed_files = vec![
            "docs/readme.md".to_string(),
            "services/api/handler.rs".to_string(),
        ];

        assert!(pr_touches_any_subpath(
            &changed_files,
            &["services/api".to_string(), "frontend".to_string()]
        ));
        assert!(!pr_touches_any_subpath(
            &changed_files,
            &["frontend".to_string(), "infra".to_string()]
        ));
    }

    #[test]
    fn normalize_repo_subpath_filters_skips_blank_repo_keys() {
        let mut filters = HashMap::new();
        filters.insert("  ".to_string(), vec!["src".to_string()]);
        filters.insert(
            "org/repo".to_string(),
            vec!["/src/".to_string(), "".to_string()],
        );

        let normalized = normalize_repo_subpath_filters(&filters);
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized.get("org/repo"), Some(&vec!["src".to_string()]));
    }
}
