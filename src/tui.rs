use crate::diff::{self, SyntaxHighlighter};
use crate::gh::{self, Comment, PullRequest};
use anyhow::Result;
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputMode {
    Normal,
    Comment,
    LineComment, // Comment on a specific line in diff
    ConfirmApprove,
    Search,      // Searching in diff
    GotoLine,    // Jump to specific line
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
    ) -> Self {
        let (async_tx, async_rx) = mpsc::channel();
        Self {
            prs: Vec::new(),
            repos_root,
            repo_list,
            username,
            include_drafts,
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
            // Debug: write full info to file
            let mut debug_output = format!("=== Line {} ===\n", line_idx);
            debug_output.push_str(&format!("delta_line_info.len() = {}\n", self.delta_line_info.len()));
            debug_output.push_str(&format!("delta_cache lines = {}\n\n", self.delta_cache.as_ref().map(|d| d.lines().count()).unwrap_or(0)));

            // Show lines around current position
            let start = line_idx.saturating_sub(5);
            let end = (line_idx + 5).min(self.delta_line_info.len().saturating_sub(1));
            if !self.delta_line_info.is_empty() && start <= end {
                for i in start..=end {
                    let marker = if i == line_idx { ">>>" } else { "   " };
                    let info = self.delta_line_info.get(i);
                    let delta_line = self.delta_cache.as_ref()
                        .and_then(|d| d.lines().nth(i))
                        .map(strip_ansi_codes)
                        .unwrap_or_default();
                    debug_output.push_str(&format!(
                        "{} [{}] file={:?} old={:?} new={:?}\n    content: {}\n",
                        marker, i,
                        info.and_then(|i| i.file_path.as_ref()),
                        info.and_then(|i| i.old_line_number),
                        info.and_then(|i| i.new_line_number),
                        delta_line
                    ));
                }
            }
            let _ = std::fs::write("/tmp/reviewer_debug.txt", &debug_output);
            self.set_status("Debug written to /tmp/reviewer_debug.txt".to_string());
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

        thread::spawn(move || {
            let prs = crate::fetch_all_prs(&repo_list, &username, include_drafts);
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
                    InputMode::Search => self.handle_search_key(key.code),
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
                KeyCode::Char('j') | KeyCode::Down => self.next(),
                KeyCode::Char('k') | KeyCode::Up => self.previous(),
                KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => self.next_page(),
                KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.previous_page()
                }
                KeyCode::PageDown => self.next_page(),
                KeyCode::PageUp => self.previous_page(),
                KeyCode::Char('g') => self.go_to_first(),
                KeyCode::Char('G') => self.go_to_last(),
                KeyCode::Home => self.go_to_first(),
                KeyCode::End => self.go_to_last(),
                KeyCode::Enter => self.enter_detail(),
                KeyCode::Char('a') => self.start_approve(),
                KeyCode::Char('R') => self.refresh(),
                KeyCode::Char('d') => self.toggle_drafts(),
                _ => {}
            },
            View::Detail => match code {
                KeyCode::Char('q') | KeyCode::Esc => self.exit_detail(),
                KeyCode::Tab => self.next_tab(),
                KeyCode::BackTab => self.prev_tab(),
                KeyCode::Char('j') | KeyCode::Down => self.scroll_down(),
                KeyCode::Char('k') | KeyCode::Up => self.scroll_up(),
                KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => self.page_down(),
                KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => self.page_up(),
                KeyCode::PageDown => self.page_down(),
                KeyCode::PageUp => self.page_up(),
                KeyCode::Char('c') => self.start_line_comment(),
                KeyCode::Char('a') => self.start_approve(),
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
            self.set_status(format!(
                "Match 1/{} for '{}'",
                self.search_matches.len(),
                self.search_query
            ));
        }
    }

    fn next_search_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_match_idx = (self.search_match_idx + 1) % self.search_matches.len();
        self.scroll_offset = self.search_matches[self.search_match_idx] as u16;
        self.set_status(format!(
            "Match {}/{} for '{}'",
            self.search_match_idx + 1,
            self.search_matches.len(),
            self.search_query
        ));
    }

    fn prev_search_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_match_idx = if self.search_match_idx == 0 {
            self.search_matches.len() - 1
        } else {
            self.search_match_idx - 1
        };
        self.scroll_offset = self.search_matches[self.search_match_idx] as u16;
        self.set_status(format!(
            "Match {}/{} for '{}'",
            self.search_match_idx + 1,
            self.search_matches.len(),
            self.search_query
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

    // Draw search input if active
    if app.input_mode == InputMode::Search {
        draw_search_input(frame, app);
    }

    // Draw goto line input if active
    if app.input_mode == InputMode::GotoLine {
        draw_goto_input(frame, app);
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
            let date = pr.updated_at.format("%m-%d");
            let mut title_spans = vec![
                Span::styled(
                    format!("[{}] ", pr.repo_name),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(format!("#{}: ", pr.number)),
            ];
            if pr.is_draft {
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
                Span::styled(date.to_string(), Style::default().fg(Color::DarkGray)),
            ]);
            ListItem::new(vec![line, details])
        })
        .collect();

    let draft_status = if app.include_drafts { " +drafts" } else { "" };
    let title = format!(" PRs requiring review ({}){} ", app.prs.len(), draft_status);
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
        " j/k: navigate | Ctrl+d/u: page | g/G: first/last | Enter: open | R: refresh | d: drafts | a: approve | q: quit",
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

    // Help - context-aware based on tab
    let help_text = if app.detail_tab == DetailTab::Diff {
        " j/k: scroll | /: search | :: goto | c: comment | D: toggle delta | a: approve | q: back"
    } else {
        " Tab: tabs | j/k: scroll | a: approve | n/p: PR | q: back"
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
) -> Result<()> {
    // Setup terminal
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let mut app = App::new(repos_root, repo_list, username, include_drafts);

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
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}
