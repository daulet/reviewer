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

enum AsyncResult {
    Diff(usize, String),           // (pr_index, diff_content)
    Comments(usize, Vec<Comment>), // (pr_index, comments)
    ClaudeLaunch(Result<String, String>), // worktree path or error
    Refresh(Vec<PullRequest>),     // refreshed PR list
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
    ConfirmApprove,
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
    pub comments_cache: Option<Vec<Comment>>,
    pub input_mode: InputMode,
    pub input_buffer: String,
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
}

impl App {
    pub fn new(repos_root: PathBuf, repo_list: Vec<PathBuf>, username: String, include_drafts: bool) -> Self {
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
            comments_cache: None,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
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

    fn enter_detail(&mut self) {
        if self.selected_pr().is_some() {
            self.view = View::Detail;
            self.detail_tab = DetailTab::Description;
            self.scroll_offset = 0;
            self.diff_cache = None;
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
        self.comments_cache = None;
        self.loading_diff = false;
        self.loading_comments = false;
        self.needs_clear = true;
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
                thread::spawn(move || {
                    let diff = gh::get_pr_diff(&pr).unwrap_or_else(|e| e.to_string());
                    let _ = tx.send(AsyncResult::Diff(idx, diff));
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
                AsyncResult::Diff(idx, diff) => {
                    // Only update if still viewing the same PR
                    if self.list_state.selected() == Some(idx) {
                        self.diff_cache = Some(diff);
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
                    let draft_status = if self.include_drafts { " (incl. drafts)" } else { "" };
                    self.set_status(format!("Refreshed: {} PRs{}", count, draft_status));
                }
            }
        }
    }

    fn start_comment(&mut self) {
        self.input_mode = InputMode::Comment;
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
                    InputMode::ConfirmApprove => self.handle_confirm_key(key.code),
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
                KeyCode::Char('c') => self.start_comment(),
                KeyCode::Char('a') => self.start_approve(),
                KeyCode::Char('r') => self.launch_claude_review(),
                KeyCode::Char('n') => {
                    self.exit_detail();
                    self.next();
                    self.enter_detail();
                }
                KeyCode::Char('p') => {
                    self.exit_detail();
                    self.previous();
                    self.enter_detail();
                }
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
        let popup = Paragraph::new(msg.as_str())
            .style(Style::default().fg(Color::Black).bg(Color::Yellow));
        frame.render_widget(popup, popup_area);
    }

    // Draw comment input if active
    if app.input_mode == InputMode::Comment {
        draw_comment_input(frame, app);
    }

    // Draw confirmation dialog if active
    if app.input_mode == InputMode::ConfirmApprove {
        draw_confirm_dialog(frame, app);
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
                title_spans.push(Span::styled("[DRAFT] ", Style::default().fg(Color::Magenta)));
            }
            title_spans.push(Span::styled(&pr.title, Style::default().add_modifier(Modifier::BOLD)));
            let line = Line::from(title_spans);
            let details = Line::from(vec![
                Span::styled(format!("  @{}", pr.author), Style::default().fg(Color::Green)),
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
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, chunks[0], &mut app.list_state);

    let help = Paragraph::new(" j/k: navigate | Enter: open | R: refresh | d: toggle drafts | a: approve | q: quit")
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

    let content_block = Block::default()
        .borders(Borders::ALL)
        .title(match app.detail_tab {
            DetailTab::Description => " Description ",
            DetailTab::Diff => " Diff ",
            DetailTab::Comments => " Comments ",
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
            let diff = if app.loading_diff {
                "Loading diff..."
            } else {
                app.diff_cache.as_deref().unwrap_or("Loading diff...")
            };
            let lines: Vec<Line> = diff
                .lines()
                .map(|line| {
                    let style = if line.starts_with('+') && !line.starts_with("+++") {
                        Style::default().fg(Color::Green)
                    } else if line.starts_with('-') && !line.starts_with("---") {
                        Style::default().fg(Color::Red)
                    } else if line.starts_with("@@") {
                        Style::default().fg(Color::Cyan)
                    } else if line.starts_with("diff ") {
                        Style::default().fg(Color::Yellow).bold()
                    } else {
                        Style::default()
                    };
                    Line::styled(line, style)
                })
                .collect();
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

    // Help
    let help = Paragraph::new(
        " Tab: tabs | j/k: scroll | r: claude review | c: comment | a: approve | n/p: PR | q: back",
    )
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

pub fn run(repos_root: PathBuf, repo_list: Vec<PathBuf>, username: String, include_drafts: bool) -> Result<()> {
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
