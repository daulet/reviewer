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
    pub list_state: ListState,
    pub view: View,
    pub detail_tab: DetailTab,
    pub scroll_offset: u16,
    pub diff_cache: Option<String>,
    pub comments_cache: Option<Vec<Comment>>,
    pub input_mode: InputMode,
    pub input_buffer: String,
    pub status_message: Option<String>,
    pub should_quit: bool,
}

impl App {
    pub fn new(prs: Vec<PullRequest>) -> Self {
        let mut list_state = ListState::default();
        if !prs.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            prs,
            list_state,
            view: View::List,
            detail_tab: DetailTab::Description,
            scroll_offset: 0,
            diff_cache: None,
            comments_cache: None,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            status_message: None,
            should_quit: false,
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
        }
    }

    fn exit_detail(&mut self) {
        self.view = View::List;
        self.scroll_offset = 0;
        self.diff_cache = None;
        self.comments_cache = None;
    }

    fn next_tab(&mut self) {
        self.detail_tab = match self.detail_tab {
            DetailTab::Description => DetailTab::Diff,
            DetailTab::Diff => DetailTab::Comments,
            DetailTab::Comments => DetailTab::Description,
        };
        self.scroll_offset = 0;
    }

    fn prev_tab(&mut self) {
        self.detail_tab = match self.detail_tab {
            DetailTab::Description => DetailTab::Comments,
            DetailTab::Diff => DetailTab::Description,
            DetailTab::Comments => DetailTab::Diff,
        };
        self.scroll_offset = 0;
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
        if self.diff_cache.is_some() {
            return;
        }
        if let Some(pr) = self.selected_pr() {
            self.diff_cache = Some(gh::get_pr_diff(pr).unwrap_or_else(|e| e.to_string()));
        }
    }

    fn load_comments(&mut self) {
        if self.comments_cache.is_some() {
            return;
        }
        if let Some(pr) = self.selected_pr() {
            self.comments_cache = Some(gh::get_pr_comments(pr).unwrap_or_default());
        }
    }

    fn start_comment(&mut self) {
        self.input_mode = InputMode::Comment;
        self.input_buffer.clear();
    }

    fn submit_comment(&mut self) {
        if self.input_buffer.trim().is_empty() {
            self.input_mode = InputMode::Normal;
            return;
        }

        if let Some(pr) = self.selected_pr().cloned() {
            match gh::add_pr_comment(&pr, &self.input_buffer) {
                Ok(()) => {
                    self.status_message = Some("Comment added successfully".to_string());
                    self.comments_cache = None; // Force reload
                }
                Err(e) => {
                    self.status_message = Some(format!("Error: {}", e));
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
                    self.status_message = Some(format!("Approved PR #{}", pr.number));
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
                    self.status_message = Some(format!("Error: {}", e));
                }
            }
        }
        self.input_mode = InputMode::Normal;
    }

    fn cancel_approve(&mut self) {
        self.input_mode = InputMode::Normal;
    }

    pub fn handle_event(&mut self) -> Result<()> {
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    return Ok(());
                }

                // Clear status message on any key
                self.status_message = None;

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
                _ => {}
            },
            View::Detail => match code {
                KeyCode::Char('q') | KeyCode::Esc => self.exit_detail(),
                KeyCode::Tab => self.next_tab(),
                KeyCode::BackTab => self.prev_tab(),
                KeyCode::Char('1') => {
                    self.detail_tab = DetailTab::Description;
                    self.scroll_offset = 0;
                }
                KeyCode::Char('2') => {
                    self.detail_tab = DetailTab::Diff;
                    self.scroll_offset = 0;
                    self.load_diff();
                }
                KeyCode::Char('3') => {
                    self.detail_tab = DetailTab::Comments;
                    self.scroll_offset = 0;
                    self.load_comments();
                }
                KeyCode::Char('j') | KeyCode::Down => self.scroll_down(),
                KeyCode::Char('k') | KeyCode::Up => self.scroll_up(),
                KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => self.page_down(),
                KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => self.page_up(),
                KeyCode::PageDown => self.page_down(),
                KeyCode::PageUp => self.page_up(),
                KeyCode::Char('c') => self.start_comment(),
                KeyCode::Char('a') => self.start_approve(),
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
    match app.view {
        View::List => draw_list(frame, app),
        View::Detail => draw_detail(frame, app),
    }

    // Draw status message if present
    if let Some(msg) = &app.status_message {
        let area = frame.area();
        let popup_area = Rect {
            x: area.width / 4,
            y: area.height - 3,
            width: area.width / 2,
            height: 3,
        };
        let popup = Paragraph::new(msg.as_str())
            .block(Block::default().borders(Borders::ALL).title("Status"))
            .style(Style::default().fg(Color::Yellow));
        frame.render_widget(Clear, popup_area);
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
            let line = Line::from(vec![
                Span::styled(
                    format!("[{}] ", pr.repo_name),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(format!("#{}: ", pr.number)),
                Span::styled(&pr.title, Style::default().add_modifier(Modifier::BOLD)),
            ]);
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

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" PRs requiring review ({}) ", app.prs.len())),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, chunks[0], &mut app.list_state);

    let help = Paragraph::new(" j/k: navigate | Enter: open | a: approve | q: quit")
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
    let tabs = Tabs::new(vec!["[1] Description", "[2] Diff", "[3] Comments"])
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

    // Content
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
            if app.diff_cache.is_none() {
                app.load_diff();
            }
            let diff = app.diff_cache.as_deref().unwrap_or("Loading...");
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
            if app.comments_cache.is_none() {
                app.load_comments();
            }
            let comments = app.comments_cache.as_ref();
            let text = match comments {
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
                None => Text::raw("Loading..."),
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
        " 1/2/3: tabs | j/k: scroll | n/p: next/prev PR | c: comment | a: approve | q: back",
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

pub fn run(prs: Vec<PullRequest>) -> Result<()> {
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

    let mut app = App::new(prs);

    // Main loop
    loop {
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
