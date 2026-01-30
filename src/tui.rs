use crate::diff::{self, SyntaxHighlighter};
use crate::gh::{self, Comment, PullRequest, ReviewState};
use anyhow::Result;
use chrono::{DateTime, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap},
    Frame,
};
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

/// Format a datetime as a human-readable age (e.g., "2h", "3d", "1w")
fn format_age(dt: &DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(*dt);

    let hours = duration.num_hours();
    let days = duration.num_days();
    let weeks = days / 7;
    let months = days / 30;

    if months > 0 {
        format!("{}mo", months)
    } else if weeks > 0 {
        format!("{}w", weeks)
    } else if days > 0 {
        format!("{}d", days)
    } else if hours > 0 {
        format!("{}h", hours)
    } else {
        "now".to_string()
    }
}

/// Strip ANSI escape codes from a string for searching
fn strip_ansi_codes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip until we hit a letter (end of escape sequence)
            while let Some(&next) = chars.peek() {
                chars.next();
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Advance search index forward with wrap-around
fn advance_search_idx(current: usize, total: usize) -> usize {
    (current + 1) % total
}

/// Retreat search index backward with wrap-around
fn retreat_search_idx(current: usize, total: usize) -> usize {
    if current == 0 {
        total - 1
    } else {
        current - 1
    }
}

/// Format search status message
fn format_search_status(idx: usize, total: usize, query: &str) -> String {
    format!("Match {}/{} for '{}'", idx + 1, total, query)
}

/// Represents a line in the parsed diff with its location info
#[derive(Debug, Clone)]
pub struct DiffLine {
    pub file_path: Option<String>,
    pub line_number: Option<u32>,     // Line number in the new file (for + and context lines)
    pub old_line_number: Option<u32>, // Line number in the old file (for - and context lines)
    #[allow(dead_code)] // Used for potential future styling
    pub line_type: DiffLineType,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiffLineType {
    Header,  // diff --git, +++, ---
    Hunk,    // @@ ... @@
    Added,   // + lines
    Removed, // - lines
    Context, // unchanged lines
    Other,
}

/// Represents a line in delta output with parsed file/line info
#[derive(Debug, Clone, Default)]
pub struct DeltaLineInfo {
    pub file_path: Option<String>,
    pub old_line_number: Option<u32>,
    pub new_line_number: Option<u32>,
}

/// Parse delta output to extract file paths and line numbers
/// Delta format: " <old_num> ⋮ <new_num> │ <content>" for code lines
/// File headers appear as plain text matching known file paths
fn parse_delta_output(delta_output: &str, raw_diff: &str) -> Vec<DeltaLineInfo> {
    let mut result = Vec::new();
    let mut current_file: Option<String> = None;

    // Extract all file paths from the raw diff
    let mut known_files: Vec<String> = Vec::new();
    for line in raw_diff.lines() {
        if line.starts_with("diff --git") {
            if let Some(b_path) = line.split(" b/").nth(1) {
                known_files.push(b_path.to_string());
            }
        }
    }

    for line in delta_output.lines() {
        let clean = strip_ansi_codes(line);
        let trimmed = clean.trim();

        // Check if this line is a file header (matches a known file path)
        for file in &known_files {
            if trimmed == file || trimmed.ends_with(file) {
                current_file = Some(file.clone());
                break;
            }
        }

        // Try to parse line numbers from delta format
        // Format 1 (unified): " <old>⋮ <new>│" for diff lines
        // Format 2 (side-by-side): "│ <old>│<content>│ <new>│<content>"
        // Format 3 (hunk header): "<num>: <content>"

        let mut old_num: Option<u32> = None;
        let mut new_num: Option<u32> = None;

        if let Some(separator_pos) = clean.find('⋮') {
            // Unified mode with ⋮ separator
            let before_sep = &clean[..separator_pos];
            let after_sep = &clean[separator_pos + '⋮'.len_utf8()..];

            old_num = before_sep
                .split_whitespace()
                .last()
                .and_then(|s| s.parse().ok());

            new_num = if let Some(pipe_pos) = after_sep.find('│') {
                after_sep[..pipe_pos]
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
            } else {
                after_sep.split_whitespace().next().and_then(|s| s.parse().ok())
            };
        } else if clean.starts_with('│') {
            // Side-by-side mode: "│ <num>│<content>│ <num>│<content>"
            // Split by │ and look for numbers
            let parts: Vec<&str> = clean.split('│').collect();
            // parts[0] is empty (before first │)
            // parts[1] might be " 130" (old line number)
            // parts[2] is content
            // parts[3] might be " 130" (new line number)
            // parts[4] is content
            if parts.len() >= 2 {
                old_num = parts[1].trim().parse().ok();
            }
            if parts.len() >= 4 {
                new_num = parts[3].trim().parse().ok();
            }
        } else if let Some(colon_pos) = clean.find(':') {
            // Hunk header format: "<num>: <content>"
            let before_colon = clean[..colon_pos].trim();
            if let Ok(line_num) = before_colon.parse::<u32>() {
                old_num = Some(line_num);
                new_num = Some(line_num);
            }
        }

        result.push(DeltaLineInfo {
            file_path: current_file.clone(),
            old_line_number: old_num,
            new_line_number: new_num,
        });
    }

    result
}

/// Parse a unified diff and extract file paths and line numbers
fn parse_diff(diff: &str) -> Vec<DiffLine> {
    let mut result = Vec::new();
    let mut current_file: Option<String> = None;
    let mut old_line_num: u32 = 0;
    let mut new_line_num: u32 = 0;

    for line in diff.lines() {
        if line.starts_with("diff --git") {
            // Extract file path from "diff --git a/path b/path"
            if let Some(b_path) = line.split(" b/").nth(1) {
                current_file = Some(b_path.to_string());
            }
            result.push(DiffLine {
                file_path: current_file.clone(),
                line_number: None,
                old_line_number: None,
                line_type: DiffLineType::Header,
            });
        } else if line.starts_with("+++") || line.starts_with("---") {
            result.push(DiffLine {
                file_path: current_file.clone(),
                line_number: None,
                old_line_number: None,
                line_type: DiffLineType::Header,
            });
        } else if line.starts_with("@@") {
            // Parse hunk header: @@ -old_start,count +new_start,count @@
            if let Some(minus_part) = line.split('-').nth(1) {
                if let Some(start_str) = minus_part.split(',').next().or_else(|| minus_part.split(' ').next()) {
                    if let Ok(start) = start_str.parse::<u32>() {
                        old_line_num = start;
                    }
                }
            }
            if let Some(plus_part) = line.split('+').nth(1) {
                if let Some(start_str) = plus_part.split(',').next().or_else(|| plus_part.split(' ').next()) {
                    if let Ok(start) = start_str.parse::<u32>() {
                        new_line_num = start;
                    }
                }
            }
            result.push(DiffLine {
                file_path: current_file.clone(),
                line_number: None,
                old_line_number: None,
                line_type: DiffLineType::Hunk,
            });
        } else if line.starts_with('+') && !line.starts_with("+++") {
            result.push(DiffLine {
                file_path: current_file.clone(),
                line_number: Some(new_line_num),
                old_line_number: None,
                line_type: DiffLineType::Added,
            });
            new_line_num += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            result.push(DiffLine {
                file_path: current_file.clone(),
                line_number: None,
                old_line_number: Some(old_line_num),
                line_type: DiffLineType::Removed,
            });
            old_line_num += 1;
        } else if line.starts_with(' ') || (!line.starts_with('\\') && !line.is_empty()) {
            // Context line (unchanged)
            result.push(DiffLine {
                file_path: current_file.clone(),
                line_number: Some(new_line_num),
                old_line_number: Some(old_line_num),
                line_type: DiffLineType::Context,
            });
            old_line_num += 1;
            new_line_num += 1;
        } else {
            result.push(DiffLine {
                file_path: current_file.clone(),
                line_number: None,
                old_line_number: None,
                line_type: DiffLineType::Other,
            });
        }
    }

    result
}

enum AsyncResult {
    Diff(usize, String, Option<String>),  // (pr_index, diff_content, delta_output)
    Comments(usize, Vec<Comment>),        // (pr_index, comments)
    ClaudeLaunch(Result<String, String>), // worktree path or error
    Refresh(Vec<PullRequest>),            // refreshed PR list
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum View {
    List,
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DetailTab {
    Description,
    Diff,
    Comments,
}

/// App mode - determines what PRs are shown
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppMode {
    /// Review mode: PRs from others needing your review
    Review,
    /// My PRs mode: Your own PRs
    MyPrs,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputMode {
    Normal,
    Comment,
    LineComment,  // Comment on a specific line in diff
    ConfirmApprove,
    ConfirmClose, // Confirm close with optional comment
    ConfirmMerge, // Confirm merge (squash)
    Search,       // Searching in diff
    ListSearch,   // Searching in PR list
    GotoLine,     // Jump to specific line
}

/// Context for a line-level comment
#[derive(Debug, Clone)]
pub struct LineCommentContext {
    pub file_path: String,
    pub line_number: u32,
    pub side: CommentSide,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CommentSide {
    Left,  // Old file (removed lines)
    Right, // New file (added/context lines)
}

pub struct App {
    pub prs: Vec<PullRequest>,
    pub repos_root: PathBuf,
    pub repo_list: Vec<PathBuf>,
    pub username: String,
    pub include_drafts: bool,
    pub mode: AppMode,
    pub list_state: ListState,
    pub view: View,
    pub detail_tab: DetailTab,
    pub scroll_offset: u16,
    pub diff_cache: Option<String>,
    pub delta_cache: Option<String>, // Pre-processed delta output (ANSI)
    pub use_delta: bool,             // Whether to use delta for rendering
    pub diff_lines: Vec<DiffLine>,   // Parsed diff with line info
    pub delta_line_info: Vec<DeltaLineInfo>, // Parsed delta output line info
    pub comments_cache: Option<Vec<Comment>>,
    pub input_mode: InputMode,
    pub input_buffer: String,
    pub line_comment_ctx: Option<LineCommentContext>, // For line-level comments
    // Search state
    pub search_query: String,
    pub search_matches: Vec<usize>, // Line indices that match
    pub search_match_idx: usize,    // Current match index
    pub status_message: Option<String>,
    pub status_time: Option<std::time::Instant>,
    pub should_quit: bool,
    // Async loading
    async_tx: Sender<AsyncResult>,
    async_rx: Receiver<AsyncResult>,
    loading_diff: bool,
    loading_comments: bool,
    refreshing: bool,
    // Screen state
    needs_clear: bool,
    // Claude launch state
    launching_claude: bool,
    // Syntax highlighter for diff rendering
    syntax_highlighter: SyntaxHighlighter,
}

impl App {
    pub fn new(
        repos_root: PathBuf,
        repo_list: Vec<PathBuf>,
        username: String,
        include_drafts: bool,
        mode: AppMode,
    ) -> Self {
        let (async_tx, async_rx) = mpsc::channel();
        Self {
            prs: Vec::new(),
            repos_root,
            repo_list,
            username,
            include_drafts,
            mode,
            list_state: ListState::default(),
            view: View::List,
            detail_tab: DetailTab::Description,
            scroll_offset: 0,
            diff_cache: None,
            delta_cache: None,
            use_delta: true, // Use delta by default if available
            diff_lines: Vec::new(),
            delta_line_info: Vec::new(),
            comments_cache: None,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            line_comment_ctx: None,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
            status_message: None,
            status_time: None,
            should_quit: false,
            async_tx,
            async_rx,
            loading_diff: false,
            loading_comments: false,
            refreshing: false,
            needs_clear: true,
            launching_claude: false,
            syntax_highlighter: SyntaxHighlighter::new(),
        }
    }

    fn set_status(&mut self, msg: String) {
        self.status_message = Some(msg);
        self.status_time = Some(std::time::Instant::now());
    }

    fn check_status_timeout(&mut self) {
        if let (Some(time), Some(_)) = (self.status_time, &self.status_message) {
            // Auto-dismiss after 3 seconds, but not while refreshing
            if !self.refreshing && time.elapsed().as_secs() >= 3 {
                self.status_message = None;
                self.status_time = None;
            }
        }
    }

    pub fn selected_pr(&self) -> Option<&PullRequest> {
        self.list_state.selected().and_then(|i| self.prs.get(i))
    }

    fn next(&mut self) {
        if self.prs.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => (i + 1) % self.prs.len(),
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn previous(&mut self) {
        if self.prs.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.prs.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn next_page(&mut self) {
        if self.prs.is_empty() {
            return;
        }
        let page_size = 10;
        let i = match self.list_state.selected() {
            Some(i) => (i + page_size).min(self.prs.len() - 1),
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn previous_page(&mut self) {
        if self.prs.is_empty() {
            return;
        }
        let page_size = 10;
        let i = match self.list_state.selected() {
            Some(i) => i.saturating_sub(page_size),
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn go_to_first(&mut self) {
        if !self.prs.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    fn go_to_last(&mut self) {
        if !self.prs.is_empty() {
            self.list_state.select(Some(self.prs.len() - 1));
        }
    }

    fn enter_detail(&mut self) {
        if self.selected_pr().is_some() {
            self.view = View::Detail;
            self.detail_tab = DetailTab::Description;
            self.scroll_offset = 0;
            self.diff_cache = None;
            self.delta_cache = None;
            self.diff_lines.clear();
            self.delta_line_info.clear();
            self.comments_cache = None;
            self.loading_diff = false;
            self.loading_comments = false;
            self.needs_clear = true;
        }
    }

    fn exit_detail(&mut self) {
        self.view = View::List;
        self.scroll_offset = 0;
        self.diff_cache = None;
        self.delta_cache = None;
        self.diff_lines.clear();
        self.delta_line_info.clear();
        self.comments_cache = None;
        self.loading_diff = false;
        self.loading_comments = false;
        self.needs_clear = true;
        self.clear_search();
    }

    fn next_tab(&mut self) {
        self.detail_tab = match self.detail_tab {
            DetailTab::Description => DetailTab::Diff,
            DetailTab::Diff => DetailTab::Comments,
            DetailTab::Comments => DetailTab::Description,
        };
        self.scroll_offset = 0;
        self.needs_clear = true;
        self.load_tab_content();
    }

    fn prev_tab(&mut self) {
        self.detail_tab = match self.detail_tab {
            DetailTab::Description => DetailTab::Comments,
            DetailTab::Diff => DetailTab::Description,
            DetailTab::Comments => DetailTab::Diff,
        };
        self.scroll_offset = 0;
        self.needs_clear = true;
        self.load_tab_content();
    }

    fn load_tab_content(&mut self) {
        match self.detail_tab {
            DetailTab::Description => {}
            DetailTab::Diff => self.load_diff(),
            DetailTab::Comments => self.load_comments(),
        }
    }

    fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(1);
    }

    fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    fn page_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(20);
    }

    fn page_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(20);
    }

    fn load_diff(&mut self) {
        if self.diff_cache.is_some() || self.loading_diff {
            return;
        }
        if let Some(idx) = self.list_state.selected() {
            if let Some(pr) = self.prs.get(idx) {
                self.loading_diff = true;
                let pr = pr.clone();
                let tx = self.async_tx.clone();
                // Get terminal width for delta's side-by-side mode
                let width = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(120);
                thread::spawn(move || {
                    let diff = gh::get_pr_diff(&pr).unwrap_or_else(|e| e.to_string());
                    // Process with delta in background
                    let delta_output = diff::process_with_delta(&diff, width);
                    let _ = tx.send(AsyncResult::Diff(idx, diff, delta_output));
                });
            }
        }
    }

    fn load_comments(&mut self) {
        if self.comments_cache.is_some() || self.loading_comments {
            return;
        }
        if let Some(idx) = self.list_state.selected() {
            if let Some(pr) = self.prs.get(idx) {
                self.loading_comments = true;
                let pr = pr.clone();
                let tx = self.async_tx.clone();
                thread::spawn(move || {
                    let comments = gh::get_pr_comments(&pr).unwrap_or_default();
                    let _ = tx.send(AsyncResult::Comments(idx, comments));
                });
            }
        }
    }

    fn poll_async_results(&mut self) {
        while let Ok(result) = self.async_rx.try_recv() {
            match result {
                AsyncResult::Diff(idx, diff, delta_output) => {
                    // Only update if still viewing the same PR
                    if self.list_state.selected() == Some(idx) {
                        self.diff_lines = parse_diff(&diff);
                        // Parse delta output for line info if available
                        if let Some(ref delta) = delta_output {
                            self.delta_line_info = parse_delta_output(delta, &diff);
                        } else {
                            self.delta_line_info.clear();
                        }
                        self.diff_cache = Some(diff);
                        self.delta_cache = delta_output;
                    }
                    self.loading_diff = false;
                }
                AsyncResult::Comments(idx, comments) => {
                    if self.list_state.selected() == Some(idx) {
                        self.comments_cache = Some(comments);
                    }
                    self.loading_comments = false;
                }
                AsyncResult::ClaudeLaunch(result) => {
                    self.launching_claude = false;
                    self.needs_clear = true;
                    match result {
                        Ok(path) => {
                            self.set_status(format!("Launched Claude in {}", path));
                        }
                        Err(e) => {
                            self.set_status(format!("Failed: {}", e));
                        }
                    }
                }
                AsyncResult::Refresh(prs) => {
                    self.refreshing = false;
                    self.needs_clear = true;
                    let count = prs.len();
                    self.prs = prs;
                    // Reset selection
                    if self.prs.is_empty() {
                        self.list_state.select(None);
                    } else {
                        self.list_state.select(Some(0));
                    }
                    let draft_status = if self.include_drafts {
                        " (incl. drafts)"
                    } else {
                        ""
                    };
                    self.set_status(format!("Refreshed: {} PRs{}", count, draft_status));
                }
            }
        }
    }

    fn start_comment(&mut self) {
        self.input_mode = InputMode::Comment;
        self.input_buffer.clear();
    }

    fn start_line_comment(&mut self) {
        // Only works in diff view with a valid line selected
        if self.detail_tab != DetailTab::Diff {
            self.start_comment(); // Fall back to general comment
            return;
        }

        let using_delta = self.use_delta && self.delta_cache.is_some();
        let line_idx = self.scroll_offset as usize;

        if using_delta {
            // Use parsed delta line info for accurate file/line lookup
            if let Some(info) = self.delta_line_info.get(line_idx) {
                if let Some(file_path) = &info.file_path {
                    // Prefer new line number (RIGHT side), fall back to old (LEFT side)
                    if let Some(line_num) = info.new_line_number {
                        self.line_comment_ctx = Some(LineCommentContext {
                            file_path: file_path.clone(),
                            line_number: line_num,
                            side: CommentSide::Right,
                        });
                        self.input_mode = InputMode::LineComment;
                        self.input_buffer.clear();
                        return;
                    }
                    if let Some(line_num) = info.old_line_number {
                        self.line_comment_ctx = Some(LineCommentContext {
                            file_path: file_path.clone(),
                            line_number: line_num,
                            side: CommentSide::Left,
                        });
                        self.input_mode = InputMode::LineComment;
                        self.input_buffer.clear();
                        return;
                    }
                }
            }
            self.set_status(
                "Cannot comment on this line. Move to a code line with line numbers.".to_string(),
            );
            return;
        }

        // Built-in mode: direct index lookup
        if let Some(diff_line) = self.diff_lines.get(line_idx) {
            if let Some(file_path) = &diff_line.file_path {
                // For added/context lines, use new file line number (RIGHT side)
                if let Some(line_num) = diff_line.line_number {
                    self.line_comment_ctx = Some(LineCommentContext {
                        file_path: file_path.clone(),
                        line_number: line_num,
                        side: CommentSide::Right,
                    });
                    self.input_mode = InputMode::LineComment;
                    self.input_buffer.clear();
                    return;
                }
                // For removed lines, use old file line number (LEFT side)
                if let Some(line_num) = diff_line.old_line_number {
                    self.line_comment_ctx = Some(LineCommentContext {
                        file_path: file_path.clone(),
                        line_number: line_num,
                        side: CommentSide::Left,
                    });
                    self.input_mode = InputMode::LineComment;
                    self.input_buffer.clear();
                    return;
                }
            }
        }

        // Fall back to general comment if no valid line
        self.set_status(
            "Cannot comment on this line. Move to an added, removed, or context line.".to_string(),
        );
    }

    fn submit_line_comment(&mut self) {
        if self.input_buffer.trim().is_empty() {
            self.input_mode = InputMode::Normal;
            self.line_comment_ctx = None;
            return;
        }

        if let (Some(pr), Some(ctx)) = (self.selected_pr().cloned(), self.line_comment_ctx.take()) {
            let side = match ctx.side {
                CommentSide::Left => "LEFT",
                CommentSide::Right => "RIGHT",
            };
            match gh::add_line_comment(&pr, &ctx.file_path, ctx.line_number, side, &self.input_buffer) {
                Ok(()) => {
                    let side_label = if ctx.side == CommentSide::Left { " (old)" } else { "" };
                    self.set_status(format!(
                        "Comment added at {}:{}{}",
                        ctx.file_path, ctx.line_number, side_label
                    ));
                }
                Err(e) => {
                    self.set_status(format!("Error: {}", e));
                }
            }
        }

        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    fn launch_claude_review(&mut self) {
        if self.launching_claude {
            return;
        }
        if let Some(pr) = self.selected_pr().cloned() {
            self.launching_claude = true;
            self.set_status("Creating worktree and launching Claude...".to_string());

            let tx = self.async_tx.clone();
            let repos_root = self.repos_root.clone();
            thread::spawn(move || {
                let result = gh::create_pr_worktree(&pr, &repos_root)
                    .and_then(|worktree_path| {
                        gh::launch_claude(&worktree_path, &pr)?;
                        Ok(worktree_path.display().to_string())
                    })
                    .map_err(|e| e.to_string());
                let _ = tx.send(AsyncResult::ClaudeLaunch(result));
            });
        }
    }

    fn submit_comment(&mut self) {
        if self.input_buffer.trim().is_empty() {
            self.input_mode = InputMode::Normal;
            return;
        }

        if let Some(pr) = self.selected_pr().cloned() {
            match gh::add_pr_comment(&pr, &self.input_buffer) {
                Ok(()) => {
                    self.set_status("Comment added successfully".to_string());
                    self.comments_cache = None; // Force reload
                }
                Err(e) => {
                    self.set_status(format!("Error: {}", e));
                }
            }
        }

        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    fn start_approve(&mut self) {
        if self.selected_pr().is_some() {
            self.input_mode = InputMode::ConfirmApprove;
        }
    }

    fn confirm_approve(&mut self) {
        if let Some(pr) = self.selected_pr().cloned() {
            match gh::approve_pr(&pr, None) {
                Ok(()) => {
                    self.set_status(format!("Approved PR #{}", pr.number));
                    // Remove from list
                    if let Some(idx) = self.list_state.selected() {
                        self.prs.remove(idx);
                        if self.prs.is_empty() {
                            self.list_state.select(None);
                            self.view = View::List;
                        } else if idx >= self.prs.len() {
                            self.list_state.select(Some(self.prs.len() - 1));
                        }
                        if self.view == View::Detail && !self.prs.is_empty() {
                            self.diff_cache = None;
                            self.comments_cache = None;
                        } else if self.prs.is_empty() {
                            self.view = View::List;
                        }
                    }
                }
                Err(e) => {
                    self.set_status(format!("Error: {}", e));
                }
            }
        }
        self.input_mode = InputMode::Normal;
    }

    fn cancel_approve(&mut self) {
        self.input_mode = InputMode::Normal;
    }

    fn start_close(&mut self) {
        if self.selected_pr().is_some() {
            self.input_mode = InputMode::ConfirmClose;
            self.input_buffer.clear();
        }
    }

    fn confirm_close(&mut self) {
        if let Some(pr) = self.selected_pr().cloned() {
            let comment = if self.input_buffer.trim().is_empty() {
                None
            } else {
                Some(self.input_buffer.as_str())
            };
            match gh::close_pr(&pr, comment) {
                Ok(()) => {
                    self.set_status(format!("Closed PR #{}", pr.number));
                    // Remove from list
                    if let Some(idx) = self.list_state.selected() {
                        self.prs.remove(idx);
                        // Adjust selection
                        if !self.prs.is_empty() {
                            let new_idx = idx.min(self.prs.len() - 1);
                            self.list_state.select(Some(new_idx));
                        } else {
                            self.list_state.select(None);
                        }
                    }
                    self.exit_detail();
                }
                Err(e) => {
                    self.set_status(format!("Error: {}", e));
                }
            }
        }
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    fn cancel_close(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    fn start_merge(&mut self) {
        // Only allow merge in MyPrs mode
        if self.mode != AppMode::MyPrs {
            self.set_status("Merge only available in --my mode".to_string());
            return;
        }

        if let Some(pr) = self.selected_pr() {
            // Check if PR can be merged
            let status = gh::check_merge_status(pr);
            if status.can_merge {
                self.input_mode = InputMode::ConfirmMerge;
            } else {
                let reason = status.reason.unwrap_or_else(|| "Unknown reason".to_string());
                self.set_status(format!("Cannot merge: {}", reason));
            }
        }
    }

    fn confirm_merge(&mut self) {
        if let Some(pr) = self.selected_pr().cloned() {
            match gh::merge_pr(&pr, true) {
                Ok(merge_type) => {
                    self.set_status(format!("Merged PR #{} ({})", pr.number, merge_type));
                    // Remove from list
                    if let Some(idx) = self.list_state.selected() {
                        self.prs.remove(idx);
                        if !self.prs.is_empty() {
                            let new_idx = idx.min(self.prs.len() - 1);
                            self.list_state.select(Some(new_idx));
                        } else {
                            self.list_state.select(None);
                        }
                    }
                    self.exit_detail();
                }
                Err(e) => {
                    self.set_status(format!("Merge failed: {}", e));
                }
            }
        }
        self.input_mode = InputMode::Normal;
    }

    fn cancel_merge(&mut self) {
        self.input_mode = InputMode::Normal;
    }

    fn refresh(&mut self) {
        if self.refreshing {
            return;
        }
        self.refreshing = true;
        self.set_status("Refreshing PR list...".to_string());

        let tx = self.async_tx.clone();
        let repo_list = self.repo_list.clone();
        let username = self.username.clone();
        let include_drafts = self.include_drafts;
        let mode = self.mode;

        thread::spawn(move || {
            let prs = match mode {
                AppMode::Review => crate::fetch_all_prs(&repo_list, &username, include_drafts),
                AppMode::MyPrs => crate::fetch_my_prs(include_drafts),
            };
            let _ = tx.send(AsyncResult::Refresh(prs));
        });
    }

    fn toggle_drafts(&mut self) {
        self.include_drafts = !self.include_drafts;
        let status = if self.include_drafts {
            "Including drafts - refreshing..."
        } else {
            "Excluding drafts - refreshing..."
        };
        self.set_status(status.to_string());
        self.refresh();
    }

    fn toggle_delta(&mut self) {
        if !diff::delta_available() {
            self.set_status("Delta not installed".to_string());
            return;
        }
        self.use_delta = !self.use_delta;
        let status = if self.use_delta {
            "Using delta renderer"
        } else {
            "Using built-in renderer (line comments enabled)"
        };
        self.set_status(status.to_string());
    }

    pub fn handle_event(&mut self) -> Result<()> {
        // Poll for async results (non-blocking)
        self.poll_async_results();

        if event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    return Ok(());
                }

                match self.input_mode {
                    InputMode::Normal => self.handle_normal_key(key.code, key.modifiers),
                    InputMode::Comment => self.handle_comment_key(key.code),
                    InputMode::LineComment => self.handle_line_comment_key(key.code),
                    InputMode::ConfirmApprove => self.handle_confirm_key(key.code),
                    InputMode::ConfirmClose => self.handle_close_key(key.code),
                    InputMode::ConfirmMerge => self.handle_merge_key(key.code),
                    InputMode::Search => self.handle_search_key(key.code),
                    InputMode::ListSearch => self.handle_list_search_key(key.code),
                    InputMode::GotoLine => self.handle_goto_key(key.code),
                }
            }
        }
        Ok(())
    }

    fn handle_normal_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match self.view {
            View::List => match code {
                KeyCode::Char('q') => self.should_quit = true,
                // Page navigation with Ctrl+d/u (must be before non-Ctrl)
                KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => self.next_page(),
                KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.previous_page()
                }
                // Regular navigation
                KeyCode::Char('j') | KeyCode::Down => self.next(),
                KeyCode::Char('k') | KeyCode::Up => self.previous(),
                KeyCode::PageDown => self.next_page(),
                KeyCode::PageUp => self.previous_page(),
                KeyCode::Char('g') => self.go_to_first(),
                KeyCode::Char('G') => self.go_to_last(),
                KeyCode::Home => self.go_to_first(),
                KeyCode::End => self.go_to_last(),
                KeyCode::Enter => self.enter_detail(),
                KeyCode::Char('R') => self.refresh(),
                KeyCode::Char('d') => self.toggle_drafts(),
                // Search in PR list
                KeyCode::Char('/') => self.start_list_search(),
                KeyCode::Char('n') if !self.search_query.is_empty() => self.next_list_search_match(),
                KeyCode::Char('N') if !self.search_query.is_empty() => self.prev_list_search_match(),
                _ => {}
            },
            View::Detail => match code {
                KeyCode::Char('q') | KeyCode::Esc => self.exit_detail(),
                KeyCode::Tab => self.next_tab(),
                KeyCode::BackTab => self.prev_tab(),
                // Page navigation with Ctrl+d/u
                KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => self.page_down(),
                KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => self.page_up(),
                // Regular navigation
                KeyCode::Char('j') | KeyCode::Down => self.scroll_down(),
                KeyCode::Char('k') | KeyCode::Up => self.scroll_up(),
                KeyCode::PageDown => self.page_down(),
                KeyCode::PageUp => self.page_up(),
                KeyCode::Char('c') => self.start_line_comment(),
                KeyCode::Char('a') => self.start_approve(),
                KeyCode::Char('x') => self.start_close(),
                KeyCode::Char('m') => self.start_merge(),
                KeyCode::Char('r') => self.launch_claude_review(),
                // Search (only in Diff tab)
                KeyCode::Char('/') if self.detail_tab == DetailTab::Diff => self.start_search(),
                KeyCode::Char('n') if !self.search_query.is_empty() => self.next_search_match(),
                KeyCode::Char('N') if !self.search_query.is_empty() => self.prev_search_match(),
                // Goto line (only in Diff tab)
                KeyCode::Char(':') if self.detail_tab == DetailTab::Diff => self.start_goto_line(),
                // Next/prev PR (when not searching)
                KeyCode::Char('n') if self.search_query.is_empty() => {
                    self.exit_detail();
                    self.next();
                    self.enter_detail();
                }
                KeyCode::Char('p') => {
                    self.exit_detail();
                    self.previous();
                    self.enter_detail();
                }
                // Toggle delta rendering (only in Diff tab)
                KeyCode::Char('D') if self.detail_tab == DetailTab::Diff => self.toggle_delta(),
                _ => {}
            },
        }
    }

    fn handle_confirm_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.confirm_approve(),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.cancel_approve(),
            _ => {}
        }
    }

    fn handle_close_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => self.confirm_close(),
            KeyCode::Esc => self.cancel_close(),
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
            }
            _ => {}
        }
    }

    fn handle_merge_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => self.confirm_merge(),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.cancel_merge(),
            _ => {}
        }
    }

    fn handle_comment_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => self.submit_comment(),
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.input_buffer.clear();
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
            }
            _ => {}
        }
    }

    fn handle_line_comment_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => self.submit_line_comment(),
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.input_buffer.clear();
                self.line_comment_ctx = None;
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
            }
            _ => {}
        }
    }

    fn handle_search_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => self.execute_search(),
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.input_buffer.clear();
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
            }
            _ => {}
        }
    }

    fn handle_list_search_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => self.execute_list_search(),
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.input_buffer.clear();
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
            }
            _ => {}
        }
    }

    fn handle_goto_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => self.execute_goto_line(),
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.input_buffer.clear();
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                self.input_buffer.push(c);
            }
            _ => {}
        }
    }

    fn start_search(&mut self) {
        self.input_mode = InputMode::Search;
        self.input_buffer.clear();
    }

    fn execute_search(&mut self) {
        if self.input_buffer.is_empty() {
            self.input_mode = InputMode::Normal;
            return;
        }

        self.search_query = self.input_buffer.clone();
        self.search_matches.clear();
        self.search_match_idx = 0;

        // Search in displayed content (delta output if available, otherwise raw diff)
        let search_content = self.delta_cache.as_ref().or(self.diff_cache.as_ref());
        if let Some(content) = search_content {
            let query_lower = self.search_query.to_lowercase();
            for (idx, line) in content.lines().enumerate() {
                // Strip ANSI codes for searching in delta output
                let clean_line = strip_ansi_codes(line);
                if clean_line.to_lowercase().contains(&query_lower) {
                    self.search_matches.push(idx);
                }
            }
        }

        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();

        if self.search_matches.is_empty() {
            self.set_status(format!("No matches for '{}'", self.search_query));
        } else {
            // Jump to first match
            self.scroll_offset = self.search_matches[0] as u16;
            self.set_status(format_search_status(0, self.search_matches.len(), &self.search_query));
        }
    }

    fn next_search_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_match_idx = advance_search_idx(self.search_match_idx, self.search_matches.len());
        self.scroll_offset = self.search_matches[self.search_match_idx] as u16;
        self.set_status(format_search_status(
            self.search_match_idx,
            self.search_matches.len(),
            &self.search_query,
        ));
    }

    fn prev_search_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_match_idx = retreat_search_idx(self.search_match_idx, self.search_matches.len());
        self.scroll_offset = self.search_matches[self.search_match_idx] as u16;
        self.set_status(format_search_status(
            self.search_match_idx,
            self.search_matches.len(),
            &self.search_query,
        ));
    }

    // List search methods
    fn start_list_search(&mut self) {
        self.input_mode = InputMode::ListSearch;
        self.input_buffer.clear();
    }

    fn execute_list_search(&mut self) {
        if self.input_buffer.is_empty() {
            self.input_mode = InputMode::Normal;
            return;
        }

        self.search_query = self.input_buffer.clone();
        self.search_matches.clear();
        self.search_match_idx = 0;

        // Search in PR titles and repo names
        let query_lower = self.search_query.to_lowercase();
        for (idx, pr) in self.prs.iter().enumerate() {
            if pr.title.to_lowercase().contains(&query_lower)
                || pr.repo_name.to_lowercase().contains(&query_lower)
                || pr.author.to_lowercase().contains(&query_lower)
            {
                self.search_matches.push(idx);
            }
        }

        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();

        if self.search_matches.is_empty() {
            self.set_status(format!("No PRs matching '{}'", self.search_query));
        } else {
            // Jump to first match
            self.list_state.select(Some(self.search_matches[0]));
            self.set_status(format_search_status(0, self.search_matches.len(), &self.search_query));
        }
    }

    fn next_list_search_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_match_idx = advance_search_idx(self.search_match_idx, self.search_matches.len());
        self.list_state.select(Some(self.search_matches[self.search_match_idx]));
        self.set_status(format_search_status(
            self.search_match_idx,
            self.search_matches.len(),
            &self.search_query,
        ));
    }

    fn prev_list_search_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_match_idx = retreat_search_idx(self.search_match_idx, self.search_matches.len());
        self.list_state.select(Some(self.search_matches[self.search_match_idx]));
        self.set_status(format_search_status(
            self.search_match_idx,
            self.search_matches.len(),
            &self.search_query,
        ));
    }

    fn start_goto_line(&mut self) {
        self.input_mode = InputMode::GotoLine;
        self.input_buffer.clear();
    }

    fn execute_goto_line(&mut self) {
        if let Ok(line_num) = self.input_buffer.parse::<u16>() {
            // Find the diff line that corresponds to this line number
            if let Some(idx) = self.diff_lines.iter().position(|dl| {
                dl.line_number.map(|n| n as u16) == Some(line_num)
            }) {
                self.scroll_offset = idx as u16;
                self.set_status(format!("Jumped to line {}", line_num));
            } else {
                // Just scroll to that offset as fallback
                self.scroll_offset = line_num.saturating_sub(1);
                self.set_status(format!("Scrolled to position {}", line_num));
            }
        }
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
    }

    fn clear_search(&mut self) {
        self.search_query.clear();
        self.search_matches.clear();
        self.search_match_idx = 0;
    }
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    // Clear screen only when view/tab/selection changed
    match app.view {
        View::List => draw_list(frame, app),
        View::Detail => draw_detail(frame, app),
    }

    // Draw status message in top right corner if present
    if let Some(msg) = &app.status_message {
        let area = frame.area();
        let msg_width = (msg.len() as u16 + 4).min(area.width / 2);
        let popup_area = Rect {
            x: area.width.saturating_sub(msg_width + 1),
            y: 0,
            width: msg_width,
            height: 1,
        };
        let popup =
            Paragraph::new(msg.as_str()).style(Style::default().fg(Color::Black).bg(Color::Yellow));
        frame.render_widget(popup, popup_area);
    }

    // Draw comment input if active
    if app.input_mode == InputMode::Comment {
        draw_comment_input(frame, app);
    }

    // Draw line comment input if active
    if app.input_mode == InputMode::LineComment {
        draw_line_comment_input(frame, app);
    }

    // Draw confirmation dialog if active
    if app.input_mode == InputMode::ConfirmApprove {
        draw_confirm_dialog(frame, app);
    }

    // Draw close dialog if active
    if app.input_mode == InputMode::ConfirmClose {
        draw_close_dialog(frame, app);
    }

    // Draw merge dialog if active
    if app.input_mode == InputMode::ConfirmMerge {
        draw_merge_dialog(frame, app);
    }

    // Draw search input if active
    if app.input_mode == InputMode::Search || app.input_mode == InputMode::ListSearch {
        draw_search_input(frame, app);
    }

    // Draw goto line input if active
    if app.input_mode == InputMode::GotoLine {
        draw_goto_input(frame, app);
    }
}

fn review_state_span(state: &ReviewState) -> Span<'static> {
    match state {
        ReviewState::Approved => Span::styled("[✓ APPROVED] ", Style::default().fg(Color::Green)),
        ReviewState::ChangesRequested => {
            Span::styled("[! CHANGES] ", Style::default().fg(Color::Red))
        }
        ReviewState::Pending => Span::styled("[○ PENDING] ", Style::default().fg(Color::Yellow)),
        ReviewState::Draft => Span::styled("[DRAFT] ", Style::default().fg(Color::Magenta)),
    }
}

fn draw_list(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(frame.area());

    let items: Vec<ListItem> = app
        .prs
        .iter()
        .map(|pr| {
            let stats = format!("+{}/-{}", pr.additions, pr.deletions);
            let age = format_age(&pr.updated_at);
            let mut title_spans = vec![
                Span::styled(
                    format!("[{}] ", pr.repo_name),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(format!("#{}: ", pr.number)),
            ];
            // Show review state in MyPrs mode, draft status in Review mode
            if app.mode == AppMode::MyPrs {
                title_spans.push(review_state_span(&pr.review_state));
            } else if pr.is_draft {
                title_spans.push(Span::styled(
                    "[DRAFT] ",
                    Style::default().fg(Color::Magenta),
                ));
            }
            title_spans.push(Span::styled(
                &pr.title,
                Style::default().add_modifier(Modifier::BOLD),
            ));
            let line = Line::from(title_spans);
            let details = Line::from(vec![
                Span::styled(
                    format!("  @{}", pr.author),
                    Style::default().fg(Color::Green),
                ),
                Span::raw(" | "),
                Span::styled(stats, Style::default().fg(Color::Yellow)),
                Span::raw(" | "),
                Span::styled(age, Style::default().fg(Color::DarkGray)),
            ]);
            ListItem::new(vec![line, details])
        })
        .collect();

    let draft_status = if app.include_drafts { " +drafts" } else { "" };
    let title = match app.mode {
        AppMode::Review => format!(" PRs requiring review ({}){} ", app.prs.len(), draft_status),
        AppMode::MyPrs => format!(" My PRs ({}){} ", app.prs.len(), draft_status),
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, chunks[0], &mut app.list_state);

    let help = Paragraph::new(
        " j/k: navigate | Ctrl+d/u: page | g/G: first/last | Enter: open | /: search | R: refresh | d: drafts | q: quit",
    )
    .style(Style::default().fg(Color::DarkGray))
    .block(Block::default().borders(Borders::ALL).title(" Help "));
    frame.render_widget(help, chunks[1]);
}

fn draw_detail(frame: &mut Frame, app: &mut App) {
    let pr = match app.selected_pr() {
        Some(pr) => pr,
        None => return,
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(frame.area());

    // Header
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            format!("[{}] ", pr.repo_name),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw(format!("#{}: ", pr.number)),
        Span::styled(&pr.title, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" by "),
        Span::styled(format!("@{}", pr.author), Style::default().fg(Color::Green)),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, chunks[0]);

    // Tabs
    let tabs = Tabs::new(vec!["Description", "Diff", "Comments"])
        .select(match app.detail_tab {
            DetailTab::Description => 0,
            DetailTab::Diff => 1,
            DetailTab::Comments => 2,
        })
        .style(Style::default().fg(Color::White))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(tabs, chunks[1]);

    // Build diff title with current line info
    let diff_title = {
        let using_delta = app.use_delta && app.delta_cache.is_some();
        let renderer = if using_delta { "delta" } else { "built-in" };
        let line_idx = app.scroll_offset as usize;
        // Only show line info when using built-in renderer (line indices match)
        if !using_delta {
            if let Some(dl) = app.diff_lines.get(line_idx) {
                if let Some(file) = &dl.file_path {
                    let line_info = match (dl.line_number, dl.old_line_number) {
                        (Some(new), _) => format!(":{}", new),
                        (None, Some(old)) => format!(":{} (old)", old),
                        _ => String::new(),
                    };
                    format!(" Diff ({}) - {}{} ", renderer, file, line_info)
                } else {
                    format!(" Diff ({}) [D to toggle] ", renderer)
                }
            } else {
                format!(" Diff ({}) [D to toggle] ", renderer)
            }
        } else {
            format!(" Diff ({}) [D to toggle] ", renderer)
        }
    };
    let content_block = Block::default()
        .borders(Borders::ALL)
        .title(match app.detail_tab {
            DetailTab::Description => " Description ".to_string(),
            DetailTab::Diff => diff_title,
            DetailTab::Comments => " Comments ".to_string(),
        });

    match app.detail_tab {
        DetailTab::Description => {
            let body = if pr.body.is_empty() {
                "No description provided.".to_string()
            } else {
                pr.body.clone()
            };
            let para = Paragraph::new(body)
                .block(content_block)
                .wrap(Wrap { trim: false })
                .scroll((app.scroll_offset, 0));
            frame.render_widget(para, chunks[2]);
        }
        DetailTab::Diff => {
            if app.diff_cache.is_none() && !app.loading_diff {
                app.load_diff();
            }
            let mut lines: Vec<Line> = if app.loading_diff {
                vec![Line::raw("Loading diff...")]
            } else if app.use_delta {
                if let Some(delta_output) = app.delta_cache.as_deref() {
                    // Use pre-processed delta output
                    diff::render_from_ansi(delta_output)
                } else if let Some(diff_content) = app.diff_cache.as_deref() {
                    // Delta not available, fallback to built-in
                    diff::render_diff(diff_content, &app.syntax_highlighter)
                } else {
                    vec![Line::raw("Loading diff...")]
                }
            } else if let Some(diff_content) = app.diff_cache.as_deref() {
                // Built-in rendering (delta disabled)
                diff::render_diff(diff_content, &app.syntax_highlighter)
            } else {
                vec![Line::raw("Loading diff...")]
            };

            // Add margin prefix to all lines, with indicator on focused line
            let focus_idx = app.scroll_offset as usize;
            for (idx, line) in lines.iter_mut().enumerate() {
                let old_line = std::mem::take(line);
                let prefix = if idx == focus_idx {
                    Span::styled("▶ ", Style::default().fg(Color::Yellow).bold())
                } else {
                    Span::raw("  ")
                };
                let mut new_spans = vec![prefix];
                new_spans.extend(old_line.spans);
                *line = if idx == focus_idx {
                    Line::from(new_spans).style(Style::default().bg(Color::DarkGray))
                } else {
                    Line::from(new_spans)
                };
            }

            let para = Paragraph::new(lines)
                .block(content_block)
                .scroll((app.scroll_offset, 0));
            frame.render_widget(para, chunks[2]);
        }
        DetailTab::Comments => {
            if app.comments_cache.is_none() && !app.loading_comments {
                app.load_comments();
            }
            let text = if app.loading_comments {
                Text::raw("Loading comments...")
            } else {
                match app.comments_cache.as_ref() {
                    Some(c) if c.is_empty() => Text::raw("No comments yet."),
                    Some(c) => {
                        let mut lines = Vec::new();
                        for comment in c {
                            let author = comment
                                .author
                                .as_ref()
                                .and_then(|a| a.login.as_ref())
                                .map(|s| s.as_str())
                                .unwrap_or("unknown");
                            let date = comment.created_at.format("%Y-%m-%d %H:%M");
                            lines.push(Line::styled(
                                format!("@{} ({})", author, date),
                                Style::default().fg(Color::Cyan).bold(),
                            ));
                            for body_line in comment.body.lines() {
                                lines.push(Line::raw(format!("  {}", body_line)));
                            }
                            lines.push(Line::raw(""));
                        }
                        Text::from(lines)
                    }
                    None => Text::raw("Loading comments..."),
                }
            };
            let para = Paragraph::new(text)
                .block(content_block)
                .wrap(Wrap { trim: false })
                .scroll((app.scroll_offset, 0));
            frame.render_widget(para, chunks[2]);
        }
    }

    // Help - context-aware based on tab and mode
    let help_text = match (app.detail_tab, app.mode) {
        (DetailTab::Diff, AppMode::MyPrs) => {
            " j/k: scroll | /: search | :: goto | D: toggle delta | m: merge | x: close | q: back"
        }
        (DetailTab::Diff, AppMode::Review) => {
            " j/k: scroll | /: search | :: goto | c: comment | D: toggle delta | a: approve | x: close | q: back"
        }
        (_, AppMode::MyPrs) => " Tab: tabs | j/k: scroll | m: merge | x: close | n/p: PR | q: back",
        (_, AppMode::Review) => " Tab: tabs | j/k: scroll | a: approve | x: close | n/p: PR | q: back",
    };
    let help = Paragraph::new(help_text)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL).title(" Help "));
    frame.render_widget(help, chunks[3]);
}

fn draw_comment_input(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let popup_area = Rect {
        x: area.width / 8,
        y: area.height / 3,
        width: area.width * 3 / 4,
        height: 5,
    };

    let input = Paragraph::new(app.input_buffer.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Add Comment (Enter to submit, Esc to cancel) ")
                .style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(Clear, popup_area);
    frame.render_widget(input, popup_area);
}

fn draw_line_comment_input(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let popup_area = Rect {
        x: area.width / 8,
        y: area.height / 3,
        width: area.width * 3 / 4,
        height: 6,
    };

    let title = if let Some(ctx) = &app.line_comment_ctx {
        format!(
            " Comment on {}:{} (Enter to submit, Esc to cancel) ",
            ctx.file_path, ctx.line_number
        )
    } else {
        " Add Line Comment (Enter to submit, Esc to cancel) ".to_string()
    };

    let input = Paragraph::new(app.input_buffer.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(Clear, popup_area);
    frame.render_widget(input, popup_area);
}

fn draw_confirm_dialog(frame: &mut Frame, app: &App) {
    let pr = match app.selected_pr() {
        Some(pr) => pr,
        None => return,
    };

    let area = frame.area();
    let popup_area = Rect {
        x: area.width / 6,
        y: area.height / 3,
        width: area.width * 2 / 3,
        height: 7,
    };

    let text = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  Approve "),
            Span::styled(
                format!("[{}] #{}", pr.repo_name, pr.number),
                Style::default().fg(Color::Cyan).bold(),
            ),
            Span::raw("?"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  [Y]", Style::default().fg(Color::Green).bold()),
            Span::raw(" Yes    "),
            Span::styled("[N]", Style::default().fg(Color::Red).bold()),
            Span::raw(" No"),
        ]),
    ];

    let dialog = Paragraph::new(text).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Confirm Approval ")
            .style(Style::default().fg(Color::Yellow)),
    );

    frame.render_widget(Clear, popup_area);
    frame.render_widget(dialog, popup_area);
}

fn draw_close_dialog(frame: &mut Frame, app: &App) {
    let pr = match app.selected_pr() {
        Some(pr) => pr,
        None => return,
    };

    let area = frame.area();
    let popup_area = Rect {
        x: area.width / 6,
        y: area.height / 3,
        width: area.width * 2 / 3,
        height: 9,
    };

    let text = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  Close "),
            Span::styled(
                format!("[{}] #{}", pr.repo_name, pr.number),
                Style::default().fg(Color::Cyan).bold(),
            ),
            Span::raw("?"),
        ]),
        Line::from(""),
        Line::from("  Optional comment:"),
        Line::from(format!("  > {}", app.input_buffer)),
        Line::from(""),
        Line::from(vec![
            Span::styled("  [Enter]", Style::default().fg(Color::Red).bold()),
            Span::raw(" Close    "),
            Span::styled("[Esc]", Style::default().fg(Color::Green).bold()),
            Span::raw(" Cancel"),
        ]),
    ];

    let dialog = Paragraph::new(text).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Close PR ")
            .style(Style::default().fg(Color::Red)),
    );

    frame.render_widget(Clear, popup_area);
    frame.render_widget(dialog, popup_area);
}

fn draw_merge_dialog(frame: &mut Frame, app: &App) {
    let pr = match app.selected_pr() {
        Some(pr) => pr,
        None => return,
    };

    let area = frame.area();
    let popup_area = Rect {
        x: area.width / 6,
        y: area.height / 3,
        width: area.width * 2 / 3,
        height: 9,
    };

    let text = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  Merge "),
            Span::styled(
                format!("[{}] #{}", pr.repo_name, pr.number),
                Style::default().fg(Color::Cyan).bold(),
            ),
            Span::raw("?"),
        ]),
        Line::from(""),
        Line::from("  Will squash if allowed, otherwise regular merge."),
        Line::from("  Branch will be deleted after merge."),
        Line::from(""),
        Line::from(vec![
            Span::styled("  [y/Enter]", Style::default().fg(Color::Green).bold()),
            Span::raw(" Merge    "),
            Span::styled("[n/Esc]", Style::default().fg(Color::Yellow).bold()),
            Span::raw(" Cancel"),
        ]),
    ];

    let dialog = Paragraph::new(text).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Merge PR ")
            .style(Style::default().fg(Color::Green)),
    );

    frame.render_widget(Clear, popup_area);
    frame.render_widget(dialog, popup_area);
}

fn draw_search_input(frame: &mut Frame, app: &App) {
    let area = frame.area();
    // Draw at bottom of screen like vim
    let popup_area = Rect {
        x: 0,
        y: area.height.saturating_sub(3),
        width: area.width,
        height: 3,
    };

    let input = Paragraph::new(format!("/{}", app.input_buffer))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Search (Enter to find, Esc to cancel) ")
                .style(Style::default().fg(Color::Yellow)),
        );

    frame.render_widget(Clear, popup_area);
    frame.render_widget(input, popup_area);
}

fn draw_goto_input(frame: &mut Frame, app: &App) {
    let area = frame.area();
    // Draw at bottom of screen like vim
    let popup_area = Rect {
        x: 0,
        y: area.height.saturating_sub(3),
        width: area.width,
        height: 3,
    };

    let input = Paragraph::new(format!(":{}", app.input_buffer))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Go to line (Enter to jump, Esc to cancel) ")
                .style(Style::default().fg(Color::Cyan)),
        );

    frame.render_widget(Clear, popup_area);
    frame.render_widget(input, popup_area);
}

pub fn run(
    repos_root: PathBuf,
    repo_list: Vec<PathBuf>,
    username: String,
    include_drafts: bool,
    mode: AppMode,
) -> Result<()> {
    // Setup terminal
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen
        // Mouse capture disabled to allow text selection in terminal
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let mut app = App::new(repos_root, repo_list, username, include_drafts, mode);

    // Start fetching PRs immediately in background
    app.refresh();

    // Main loop
    loop {
        // Force full terminal clear when needed
        if app.needs_clear {
            terminal.clear()?;
            app.needs_clear = false;
        }

        // Auto-dismiss status messages after timeout
        app.check_status_timeout();

        terminal.draw(|f| draw(f, &mut app))?;
        app.handle_event()?;

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests use output patterns captured from real `delta` CLI output

    #[test]
    fn test_parse_delta_unified_mode() {
        // Real unified mode format from: delta --line-numbers
        // Format: " <old>⋮ <new>│<content>"
        let raw_diff = r#"diff --git a/src/gh.rs b/src/gh.rs
--- a/src/gh.rs
+++ b/src/gh.rs
@@ -303,7 +303,8 @@ pub fn add_pr_comment
 }

 /// Add a line-level comment
-pub fn add_line_comment(old)
+/// side doc
+pub fn add_line_comment(new)"#;

        // Captured from: git diff | delta --line-numbers | sed 's/\x1b\[[0-9;]*m//g'
        let delta_output = r#"
src/gh.rs
────────────────────────────────────────────────────────────────────────────────

────────────────────────────────────────────────────────────────────────────┐
303: pub fn add_pr_comment(pr: &PullRequest, comment: &str) -> Result<()> { │
────────────────────────────────────────────────────────────────────────────┘
 303⋮ 303│}
 304⋮ 304│
 305⋮ 305│/// Add a line-level comment to a PR using the reviews API
 306⋮    │pub fn add_line_comment(pr: &PullRequest, file_path: &str, line: u32, comment: &str) -> Result<()> {
    ⋮ 306│/// `side` should be "LEFT" for removed lines (old file) or "RIGHT" for added/context lines (new file)
    ⋮ 307│pub fn add_line_comment(pr: &PullRequest, file_path: &str, line: u32, side: &str, comment: &str) -> Result<()> {
 307⋮ 308│    // Use the reviews endpoint with a comments array"#;

        let result = parse_delta_output(delta_output, raw_diff);

        // Line 0: empty
        assert_eq!(result[0].file_path, None);

        // Line 1: file header "src/gh.rs"
        assert_eq!(result[1].file_path.as_deref(), Some("src/gh.rs"));

        // Line 5: hunk header "303: pub fn..."
        assert_eq!(result[5].old_line_number, Some(303));
        assert_eq!(result[5].new_line_number, Some(303));

        // Line 7: " 303⋮ 303│" - context line
        assert_eq!(result[7].file_path.as_deref(), Some("src/gh.rs"));
        assert_eq!(result[7].old_line_number, Some(303));
        assert_eq!(result[7].new_line_number, Some(303));

        // Line 10: " 306⋮    │" - removed line (has old, no new)
        assert_eq!(result[10].old_line_number, Some(306));
        assert_eq!(result[10].new_line_number, None);

        // Line 11: "    ⋮ 306│" - added line (no old, has new)
        assert_eq!(result[11].old_line_number, None);
        assert_eq!(result[11].new_line_number, Some(306));

        // Line 13: " 307⋮ 308│" - context line (shifted)
        assert_eq!(result[13].old_line_number, Some(307));
        assert_eq!(result[13].new_line_number, Some(308));
    }

    #[test]
    fn test_parse_delta_side_by_side_mode() {
        // Real side-by-side format from: delta --side-by-side --line-numbers
        // Format: "│ <old>│<content>│ <new>│<content>"
        let raw_diff = r#"diff --git a/src/gh.rs b/src/gh.rs
--- a/src/gh.rs
+++ b/src/gh.rs
@@ -303,4 +303,5 @@ pub fn add_pr_comment
 }

+/// new comment
 /// Add a line-level comment"#;

        // Captured from: git diff | delta --side-by-side --line-numbers | sed 's/\x1b\[[0-9;]*m//g'
        let delta_output = r#"
src/gh.rs
────────────────────────────────────────────────────────────────────────────────

────────────────────────────────────────────────────────────────────────────┐
303: pub fn add_pr_comment(pr: &PullRequest, comment: &str) -> Result<()> { │
────────────────────────────────────────────────────────────────────────────┘
│ 303│}                                 │ 303│}
│ 304│                                  │ 304│
│    │                                  │ 305│/// new comment
│ 305│/// Add a line-level comment      │ 306│/// Add a line-level comment"#;

        let result = parse_delta_output(delta_output, raw_diff);

        // Line 1: file header
        assert_eq!(result[1].file_path.as_deref(), Some("src/gh.rs"));

        // Line 7: "│ 303│...│ 303│..." - context line
        assert_eq!(result[7].file_path.as_deref(), Some("src/gh.rs"));
        assert_eq!(result[7].old_line_number, Some(303));
        assert_eq!(result[7].new_line_number, Some(303));

        // Line 9: "│    │...│ 305│..." - added line (no old, has new)
        assert_eq!(result[9].old_line_number, None);
        assert_eq!(result[9].new_line_number, Some(305));

        // Line 10: "│ 305│...│ 306│..." - context line (shifted)
        assert_eq!(result[10].old_line_number, Some(305));
        assert_eq!(result[10].new_line_number, Some(306));
    }

    #[test]
    fn test_parse_delta_side_by_side_removed_line() {
        // Side-by-side with removed lines
        let raw_diff = r#"diff --git a/test.rs b/test.rs
--- a/test.rs
+++ b/test.rs
@@ -1,3 +1,2 @@
 keep
-removed
 also keep"#;

        let delta_output = r#"
test.rs
────────────────────────────────────────
│   1│keep                              │   1│keep
│   2│removed                           │    │
│   3│also keep                         │   2│also keep"#;

        let result = parse_delta_output(delta_output, raw_diff);

        // Line 3: context line
        assert_eq!(result[3].old_line_number, Some(1));
        assert_eq!(result[3].new_line_number, Some(1));

        // Line 4: removed line (has old, no new)
        assert_eq!(result[4].old_line_number, Some(2));
        assert_eq!(result[4].new_line_number, None);

        // Line 5: context line (shifted)
        assert_eq!(result[5].old_line_number, Some(3));
        assert_eq!(result[5].new_line_number, Some(2));
    }

    #[test]
    fn test_parse_delta_hunk_header() {
        // Hunk header format: "<num>: <content>" inside box decorations
        let raw_diff = r#"diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -50,2 +50,3 @@ fn helper() {
     code();
+    more();
}"#;

        let delta_output = r#"
src/lib.rs
────────────────────────────────────────
────────────────┐
50: fn helper() { │
────────────────┘
 50⋮ 50│    code();
    ⋮ 51│    more();
 51⋮ 52│}"#;

        let result = parse_delta_output(delta_output, raw_diff);

        // Line 4: hunk header "50: fn helper()"
        assert_eq!(result[4].file_path.as_deref(), Some("src/lib.rs"));
        assert_eq!(result[4].old_line_number, Some(50));
        assert_eq!(result[4].new_line_number, Some(50));

        // Line 6: code line
        assert_eq!(result[6].old_line_number, Some(50));
        assert_eq!(result[6].new_line_number, Some(50));
    }

    #[test]
    fn test_parse_delta_multiple_files() {
        let raw_diff = r#"diff --git a/file1.rs b/file1.rs
--- a/file1.rs
+++ b/file1.rs
@@ -1 +1 @@
-old1
+new1
diff --git a/file2.rs b/file2.rs
--- a/file2.rs
+++ b/file2.rs
@@ -1 +1 @@
-old2
+new2"#;

        let delta_output = r#"
file1.rs
────────────────────────────────────────
  1⋮  1│new1

file2.rs
────────────────────────────────────────
  1⋮  1│new2"#;

        let result = parse_delta_output(delta_output, raw_diff);

        // Find first file's code line
        let file1_line = result.iter().find(|r| {
            r.file_path.as_deref() == Some("file1.rs") && r.new_line_number.is_some()
        });
        assert!(file1_line.is_some());
        assert_eq!(file1_line.unwrap().new_line_number, Some(1));

        // Find second file's code line - should switch to file2.rs
        let file2_line = result.iter().find(|r| {
            r.file_path.as_deref() == Some("file2.rs") && r.new_line_number.is_some()
        });
        assert!(file2_line.is_some());
        assert_eq!(file2_line.unwrap().new_line_number, Some(1));
    }

    #[test]
    fn test_parse_delta_decoration_lines() {
        // Decoration lines (separators, box corners) should have no line numbers
        let raw_diff = r#"diff --git a/test.rs b/test.rs
--- a/test.rs
+++ b/test.rs
@@ -1 +1 @@
 code"#;

        let delta_output = r#"
test.rs
────────────────────────────────────────

────────────────┐
1: fn test() {   │
────────────────┘
  1⋮  1│code"#;

        let result = parse_delta_output(delta_output, raw_diff);

        // Line 2: separator line (────...)
        assert_eq!(result[2].old_line_number, None);
        assert_eq!(result[2].new_line_number, None);

        // Line 3: empty line
        assert_eq!(result[3].old_line_number, None);
        assert_eq!(result[3].new_line_number, None);

        // Line 4: box top (────...┐)
        assert_eq!(result[4].old_line_number, None);
        assert_eq!(result[4].new_line_number, None);

        // Line 6: box bottom (────...┘)
        assert_eq!(result[6].old_line_number, None);
        assert_eq!(result[6].new_line_number, None);

        // Line 7: actual code line
        assert_eq!(result[7].old_line_number, Some(1));
        assert_eq!(result[7].new_line_number, Some(1));
    }

    #[test]
    fn test_strip_ansi_codes() {
        // Test ANSI code stripping with real delta escape sequences
        let with_ansi = "\x1b[34msrc/main.rs\x1b[0m";
        assert_eq!(strip_ansi_codes(with_ansi), "src/main.rs");

        // Real delta output: colored line numbers
        let complex = "\x1b[38;2;68;68;68m 303⋮ 303\x1b[34m│\x1b[0m}";
        let stripped = strip_ansi_codes(complex);
        assert!(stripped.contains("303"));
        assert!(stripped.contains("⋮"));
        assert!(stripped.contains("│"));
    }

    // ==================== Tests for parse_diff (non-delta built-in mode) ====================

    #[test]
    fn test_parse_diff_basic() {
        let diff = r#"diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,4 +10,5 @@ fn main() {
     let x = 1;
+    let y = 2;
     println!("test");
 }"#;

        let result = parse_diff(diff);

        // Line 0: diff --git header
        assert_eq!(result[0].file_path.as_deref(), Some("src/main.rs"));
        assert_eq!(result[0].line_type, DiffLineType::Header);

        // Line 1-2: --- and +++ headers
        assert_eq!(result[1].line_type, DiffLineType::Header);
        assert_eq!(result[2].line_type, DiffLineType::Header);

        // Line 3: @@ hunk header
        assert_eq!(result[3].line_type, DiffLineType::Hunk);

        // Line 4: context line "     let x = 1;"
        assert_eq!(result[4].line_type, DiffLineType::Context);
        assert_eq!(result[4].old_line_number, Some(10));
        assert_eq!(result[4].line_number, Some(10));

        // Line 5: added line "+    let y = 2;"
        assert_eq!(result[5].line_type, DiffLineType::Added);
        assert_eq!(result[5].old_line_number, None);
        assert_eq!(result[5].line_number, Some(11));

        // Line 6: context line "     println!..."
        assert_eq!(result[6].line_type, DiffLineType::Context);
        assert_eq!(result[6].old_line_number, Some(11));
        assert_eq!(result[6].line_number, Some(12));
    }

    #[test]
    fn test_parse_diff_removed_lines() {
        let diff = r#"diff --git a/test.rs b/test.rs
--- a/test.rs
+++ b/test.rs
@@ -1,4 +1,3 @@
 fn main() {
-    let old = 1;
     let keep = 2;
 }"#;

        let result = parse_diff(diff);

        // Line 4: context "fn main()"
        assert_eq!(result[4].line_type, DiffLineType::Context);
        assert_eq!(result[4].old_line_number, Some(1));
        assert_eq!(result[4].line_number, Some(1));

        // Line 5: removed line (has old_line_number, no line_number)
        assert_eq!(result[5].line_type, DiffLineType::Removed);
        assert_eq!(result[5].old_line_number, Some(2));
        assert_eq!(result[5].line_number, None);

        // Line 6: context "let keep" - shifted in new file
        assert_eq!(result[6].line_type, DiffLineType::Context);
        assert_eq!(result[6].old_line_number, Some(3));
        assert_eq!(result[6].line_number, Some(2));
    }

    #[test]
    fn test_parse_diff_multiple_files() {
        let diff = r#"diff --git a/file1.rs b/file1.rs
--- a/file1.rs
+++ b/file1.rs
@@ -1,2 +1,2 @@
-old content
+new content
diff --git a/file2.rs b/file2.rs
--- a/file2.rs
+++ b/file2.rs
@@ -5,2 +5,3 @@
 existing
+added line"#;

        let result = parse_diff(diff);

        // First file
        assert_eq!(result[0].file_path.as_deref(), Some("file1.rs"));
        assert_eq!(result[4].file_path.as_deref(), Some("file1.rs"));
        assert_eq!(result[4].line_type, DiffLineType::Removed);
        assert_eq!(result[5].line_type, DiffLineType::Added);

        // Second file - should switch file path
        assert_eq!(result[6].file_path.as_deref(), Some("file2.rs"));
        assert_eq!(result[6].line_type, DiffLineType::Header);

        // Lines in second file start at line 5
        assert_eq!(result[10].line_type, DiffLineType::Context);
        assert_eq!(result[10].old_line_number, Some(5));
        assert_eq!(result[11].line_type, DiffLineType::Added);
        assert_eq!(result[11].line_number, Some(6));
    }

    #[test]
    fn test_parse_diff_hunk_header_parsing() {
        // Test various hunk header formats
        let diff = r#"diff --git a/test.rs b/test.rs
--- a/test.rs
+++ b/test.rs
@@ -100,5 +200,10 @@ fn context_function() {
 context line"#;

        let result = parse_diff(diff);

        // After hunk header, lines should start at correct numbers
        // Old file: starts at 100, new file: starts at 200
        assert_eq!(result[4].line_type, DiffLineType::Context);
        assert_eq!(result[4].old_line_number, Some(100));
        assert_eq!(result[4].line_number, Some(200));
    }

    #[test]
    fn test_parse_diff_no_newline_marker() {
        // "\ No newline at end of file" should be treated as Other
        let diff = r#"diff --git a/test.rs b/test.rs
--- a/test.rs
+++ b/test.rs
@@ -1,2 +1,2 @@
-old
+new
\ No newline at end of file"#;

        let result = parse_diff(diff);

        // The backslash line should be Other type
        assert_eq!(result[6].line_type, DiffLineType::Other);
        assert_eq!(result[6].line_number, None);
        assert_eq!(result[6].old_line_number, None);
    }

    #[test]
    fn test_parse_diff_file_path_extraction() {
        // Test file path extraction from various diff formats
        let diff = r#"diff --git a/path/to/file.rs b/path/to/file.rs
--- a/path/to/file.rs
+++ b/path/to/file.rs
@@ -1 +1 @@
-old
+new"#;

        let result = parse_diff(diff);

        // All lines should have the correct file path
        for line in &result {
            assert_eq!(line.file_path.as_deref(), Some("path/to/file.rs"));
        }
    }

    // ==================== Tests for search index helpers ====================

    #[test]
    fn test_advance_search_idx() {
        // Test forward cycling through indices
        assert_eq!(advance_search_idx(0, 5), 1);
        assert_eq!(advance_search_idx(4, 5), 0); // wrap around
        assert_eq!(advance_search_idx(0, 1), 0); // single element
    }

    #[test]
    fn test_retreat_search_idx() {
        // Test backward cycling through indices
        assert_eq!(retreat_search_idx(1, 5), 0);
        assert_eq!(retreat_search_idx(0, 5), 4); // wrap around
        assert_eq!(retreat_search_idx(0, 1), 0); // single element
    }

    #[test]
    fn test_format_search_status() {
        assert_eq!(
            format_search_status(0, 5, "test"),
            "Match 1/5 for 'test'"
        );
        assert_eq!(
            format_search_status(4, 5, "foo"),
            "Match 5/5 for 'foo'"
        );
    }
}
