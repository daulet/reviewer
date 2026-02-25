use ansi_to_tui::IntoText;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use similar::{ChangeTag, TextDiff};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use syntect::{
    highlighting::{Theme, ThemeSet},
    parsing::SyntaxSet,
};

const DELTA_DIFF_SIZE_LIMIT: usize = 100_000;

/// Check if delta is available on the system (cached)
fn is_delta_available() -> bool {
    static DELTA_AVAILABLE: OnceLock<bool> = OnceLock::new();
    *DELTA_AVAILABLE.get_or_init(|| {
        Command::new("delta")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// Pipe diff content through delta and return ANSI-colored output
fn run_delta(diff: &str, width: u16) -> Option<String> {
    use std::time::Duration;

    // Skip delta for very large diffs (>100KB) to avoid slow processing
    if is_too_large_for_delta(diff) {
        return None;
    }

    let mut child = Command::new("delta")
        .args([
            "--dark",
            "--paging=never",
            "--line-numbers",
            "--side-by-side",
            &format!("--width={width}"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    // Write to stdin and close it
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(diff.as_bytes());
        // stdin is dropped here, closing the pipe
    }

    // Wait with timeout using a separate thread
    let timeout = Duration::from_secs(10);
    let handle = std::thread::spawn(move || child.wait_with_output());

    // Wait for the thread with timeout
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if handle.is_finished() {
            return handle
                .join()
                .ok()
                .and_then(|r| r.ok())
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Timeout - we can't easily kill the child from here, but the thread will eventually finish
    None
}

/// Process diff through delta asynchronously (call from background thread)
/// Returns Some(ansi_output) if delta is available, None otherwise
pub fn process_with_delta(diff: &str, width: u16) -> Option<String> {
    if is_delta_available() {
        run_delta(diff, width)
    } else {
        None
    }
}

/// Returns true when diff content exceeds the limit we allow delta to process.
pub fn is_too_large_for_delta(diff: &str) -> bool {
    diff.len() > DELTA_DIFF_SIZE_LIMIT
}

/// Convert a Line with borrowed content to owned content
fn line_to_owned(line: Line<'_>) -> Line<'static> {
    Line::from(
        line.spans
            .into_iter()
            .map(|span| Span::styled(span.content.to_string(), span.style))
            .collect::<Vec<_>>(),
    )
}

/// Render ANSI-colored string to ratatui Lines (for cached delta output)
pub fn render_from_ansi(ansi_text: &str) -> Vec<Line<'static>> {
    match ansi_text.into_text() {
        Ok(text) => text.lines.into_iter().map(line_to_owned).collect(),
        Err(_) => {
            // Fallback: just split by newlines without ANSI parsing
            ansi_text
                .lines()
                .map(|l| Line::raw(l.to_string()))
                .collect()
        }
    }
}

/// Check if delta is available for rendering (public API)
pub fn delta_available() -> bool {
    is_delta_available()
}

/// Holds syntax highlighting state
pub struct SyntaxHighlighter {
    syntax_set: SyntaxSet,
    theme: Theme,
}

impl Default for SyntaxHighlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl SyntaxHighlighter {
    pub fn new() -> Self {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let theme_set = ThemeSet::load_defaults();
        // Use a dark theme suitable for terminals
        let theme = theme_set.themes["base16-ocean.dark"].clone();
        Self { syntax_set, theme }
    }

    /// Get syntax-highlighted spans for a line of code
    pub fn highlight_line(&self, line: &str, extension: &str) -> Vec<Span<'static>> {
        use syntect::easy::HighlightLines;
        use syntect::util::LinesWithEndings;

        let syntax = self
            .syntax_set
            .find_syntax_by_extension(extension)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        let mut highlighter = HighlightLines::new(syntax, &self.theme);

        let mut spans = Vec::new();

        // Highlight each line segment
        for line_content in LinesWithEndings::from(line) {
            if let Ok(ranges) = highlighter.highlight_line(line_content, &self.syntax_set) {
                for (style, text) in ranges {
                    let fg = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
                    spans.push(Span::styled(text.to_string(), Style::default().fg(fg)));
                }
            }
        }

        if spans.is_empty() {
            spans.push(Span::raw(line.to_string()));
        }

        spans
    }
}

/// Parsed diff with enhanced rendering information
#[derive(Debug, Clone)]
pub struct EnhancedDiffLine {
    pub content: String,
    pub line_type: DiffLineType,
    pub file_path: Option<String>,
    pub old_line_num: Option<u32>,
    pub new_line_num: Option<u32>,
    /// Word-level changes within this line (for added/removed lines)
    pub word_changes: Vec<WordChange>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiffLineType {
    FileHeader, // diff --git line
    OldFile,    // --- line
    NewFile,    // +++ line
    Hunk,       // @@ ... @@
    Added,      // + lines
    Removed,    // - lines
    Context,    // unchanged lines
    NoNewline,  // \ No newline at end of file
}

#[derive(Debug, Clone)]
pub struct WordChange {
    pub start: usize,
    pub end: usize,
    pub emphasized: bool, // true = this part changed
}

/// Parse a unified diff into enhanced diff lines
pub fn parse_diff_enhanced(diff: &str) -> Vec<EnhancedDiffLine> {
    let mut result = Vec::new();
    let mut current_file: Option<String> = None;
    let mut old_line_num: u32 = 0;
    let mut new_line_num: u32 = 0;

    // Collect hunks for word-level diff computation
    let mut pending_removed: Vec<(usize, String)> = Vec::new();
    let mut pending_added: Vec<(usize, String)> = Vec::new();

    let lines: Vec<&str> = diff.lines().collect();

    for (idx, line) in lines.iter().enumerate() {
        let line_type;
        let old_num;
        let new_num;

        if line.starts_with("diff --git") {
            // Flush pending changes
            compute_word_changes(&mut result, &pending_removed, &pending_added);
            pending_removed.clear();
            pending_added.clear();

            // Extract file path from "diff --git a/path b/path"
            if let Some(b_path) = line.split(" b/").nth(1) {
                current_file = Some(b_path.to_string());
            }
            line_type = DiffLineType::FileHeader;
            old_num = None;
            new_num = None;
        } else if line.starts_with("---") {
            compute_word_changes(&mut result, &pending_removed, &pending_added);
            pending_removed.clear();
            pending_added.clear();
            line_type = DiffLineType::OldFile;
            old_num = None;
            new_num = None;
        } else if line.starts_with("+++") {
            line_type = DiffLineType::NewFile;
            old_num = None;
            new_num = None;
        } else if line.starts_with("@@") {
            compute_word_changes(&mut result, &pending_removed, &pending_added);
            pending_removed.clear();
            pending_added.clear();

            // Parse hunk header: @@ -old_start,old_count +new_start,new_count @@
            if let Some((old_start, new_start)) = parse_hunk_header(line) {
                old_line_num = old_start;
                new_line_num = new_start;
            }
            line_type = DiffLineType::Hunk;
            old_num = None;
            new_num = None;
        } else if let Some(stripped) = line.strip_prefix('+') {
            line_type = DiffLineType::Added;
            old_num = None;
            new_num = Some(new_line_num);
            new_line_num += 1;
            pending_added.push((result.len(), stripped.to_string()));
        } else if let Some(stripped) = line.strip_prefix('-') {
            line_type = DiffLineType::Removed;
            old_num = Some(old_line_num);
            new_num = None;
            old_line_num += 1;
            pending_removed.push((result.len(), stripped.to_string()));
        } else if line.starts_with('\\') {
            line_type = DiffLineType::NoNewline;
            old_num = None;
            new_num = None;
        } else {
            // Context line or empty
            compute_word_changes(&mut result, &pending_removed, &pending_added);
            pending_removed.clear();
            pending_added.clear();

            line_type = DiffLineType::Context;
            old_num = Some(old_line_num);
            new_num = Some(new_line_num);
            old_line_num += 1;
            new_line_num += 1;
        }

        result.push(EnhancedDiffLine {
            content: line.to_string(),
            line_type,
            file_path: current_file.clone(),
            old_line_num: old_num,
            new_line_num: new_num,
            word_changes: Vec::new(),
        });

        // Look ahead: if next line breaks the add/remove sequence, compute changes
        let next_continues = idx + 1 < lines.len() && {
            let next = lines[idx + 1];
            next.starts_with('+') || next.starts_with('-')
        };

        if !next_continues && (!pending_removed.is_empty() || !pending_added.is_empty()) {
            compute_word_changes(&mut result, &pending_removed, &pending_added);
            pending_removed.clear();
            pending_added.clear();
        }
    }

    // Final flush
    compute_word_changes(&mut result, &pending_removed, &pending_added);

    result
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    // @@ -old_start,old_count +new_start,new_count @@ optional context
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }

    let old_part = parts[1].trim_start_matches('-');
    let new_part = parts[2].trim_start_matches('+');

    let old_start = old_part
        .split(',')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let new_start = new_part
        .split(',')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    Some((old_start, new_start))
}

/// Compute word-level changes between removed and added lines
fn compute_word_changes(
    result: &mut [EnhancedDiffLine],
    removed: &[(usize, String)],
    added: &[(usize, String)],
) {
    if removed.is_empty() || added.is_empty() {
        return;
    }

    // Simple case: one removed line, one added line - do word diff
    if removed.len() == 1 && added.len() == 1 {
        let (rem_idx, rem_text) = &removed[0];
        let (add_idx, add_text) = &added[0];

        let diff = TextDiff::from_words(rem_text.as_str(), add_text.as_str());

        let mut rem_changes = Vec::new();
        let mut add_changes = Vec::new();
        let mut rem_pos = 0;
        let mut add_pos = 0;

        for change in diff.iter_all_changes() {
            let len = change.value().len();
            match change.tag() {
                ChangeTag::Equal => {
                    rem_changes.push(WordChange {
                        start: rem_pos,
                        end: rem_pos + len,
                        emphasized: false,
                    });
                    add_changes.push(WordChange {
                        start: add_pos,
                        end: add_pos + len,
                        emphasized: false,
                    });
                    rem_pos += len;
                    add_pos += len;
                }
                ChangeTag::Delete => {
                    rem_changes.push(WordChange {
                        start: rem_pos,
                        end: rem_pos + len,
                        emphasized: true,
                    });
                    rem_pos += len;
                }
                ChangeTag::Insert => {
                    add_changes.push(WordChange {
                        start: add_pos,
                        end: add_pos + len,
                        emphasized: true,
                    });
                    add_pos += len;
                }
            }
        }

        if let Some(line) = result.get_mut(*rem_idx) {
            line.word_changes = rem_changes;
        }
        if let Some(line) = result.get_mut(*add_idx) {
            line.word_changes = add_changes;
        }
    }
}

/// Get file extension from a file path
pub fn get_extension(file_path: &str) -> &str {
    Path::new(file_path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
}

/// Render a diff line to ratatui Line with syntax highlighting and word-level emphasis
pub fn render_diff_line<'a>(
    diff_line: &EnhancedDiffLine,
    highlighter: &SyntaxHighlighter,
    line_number_width: usize,
) -> Line<'a> {
    let ext = diff_line
        .file_path
        .as_ref()
        .map(|p| get_extension(p))
        .unwrap_or("");

    match diff_line.line_type {
        DiffLineType::FileHeader => Line::styled(
            diff_line.content.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        DiffLineType::OldFile | DiffLineType::NewFile => Line::styled(
            diff_line.content.clone(),
            Style::default().fg(Color::Yellow),
        ),
        DiffLineType::Hunk => {
            Line::styled(diff_line.content.clone(), Style::default().fg(Color::Cyan))
        }
        DiffLineType::Added => {
            let prefix = format_line_numbers(None, diff_line.new_line_num, line_number_width);
            let content = &diff_line.content[1..]; // Skip the '+'

            let mut spans = vec![
                Span::styled(prefix, Style::default().fg(Color::DarkGray)),
                Span::styled("+", Style::default().fg(Color::Green)),
            ];

            if diff_line.word_changes.is_empty() {
                // No word-level diff, apply syntax highlighting
                let mut highlighted = highlighter.highlight_line(content, ext);
                for span in &mut highlighted {
                    // Tint all spans green for added lines
                    span.style = span.style.bg(Color::Rgb(0, 40, 0));
                }
                spans.extend(highlighted);
            } else {
                // Word-level diff: emphasize changed parts
                spans.extend(render_word_changes(
                    content,
                    &diff_line.word_changes,
                    Color::Green,
                    Color::Rgb(0, 80, 0),
                ));
            }

            Line::from(spans)
        }
        DiffLineType::Removed => {
            let prefix = format_line_numbers(diff_line.old_line_num, None, line_number_width);
            let content = &diff_line.content[1..]; // Skip the '-'

            let mut spans = vec![
                Span::styled(prefix, Style::default().fg(Color::DarkGray)),
                Span::styled("-", Style::default().fg(Color::Red)),
            ];

            if diff_line.word_changes.is_empty() {
                // No word-level diff, apply syntax highlighting
                let mut highlighted = highlighter.highlight_line(content, ext);
                for span in &mut highlighted {
                    // Tint all spans red for removed lines
                    span.style = span.style.bg(Color::Rgb(40, 0, 0));
                }
                spans.extend(highlighted);
            } else {
                // Word-level diff: emphasize changed parts
                spans.extend(render_word_changes(
                    content,
                    &diff_line.word_changes,
                    Color::Red,
                    Color::Rgb(80, 0, 0),
                ));
            }

            Line::from(spans)
        }
        DiffLineType::Context => {
            let prefix = format_line_numbers(
                diff_line.old_line_num,
                diff_line.new_line_num,
                line_number_width,
            );
            let content = if diff_line.content.is_empty() {
                ""
            } else {
                &diff_line.content[1..] // Skip the leading space
            };

            let mut spans = vec![
                Span::styled(prefix, Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
            ];
            spans.extend(highlighter.highlight_line(content, ext));

            Line::from(spans)
        }
        DiffLineType::NoNewline => Line::styled(
            diff_line.content.clone(),
            Style::default().fg(Color::DarkGray),
        ),
    }
}

fn format_line_numbers(old: Option<u32>, new: Option<u32>, width: usize) -> String {
    let old_str = old
        .map(|n| format!("{:>width$}", n, width = width))
        .unwrap_or_else(|| " ".repeat(width));
    let new_str = new
        .map(|n| format!("{:>width$}", n, width = width))
        .unwrap_or_else(|| " ".repeat(width));
    format!("{} {} ", old_str, new_str)
}

fn render_word_changes(
    content: &str,
    changes: &[WordChange],
    base_color: Color,
    emphasis_bg: Color,
) -> Vec<Span<'static>> {
    if changes.is_empty() {
        return vec![Span::styled(
            content.to_string(),
            Style::default().fg(base_color),
        )];
    }

    let mut spans = Vec::new();
    let mut last_end = 0;

    for change in changes {
        // Text before this change (if any gap)
        if change.start > last_end {
            let text = &content[last_end..change.start.min(content.len())];
            spans.push(Span::styled(
                text.to_string(),
                Style::default().fg(base_color),
            ));
        }

        // The changed text
        let start = change.start.min(content.len());
        let end = change.end.min(content.len());
        if start < end {
            let text = &content[start..end];
            let style = if change.emphasized {
                Style::default()
                    .fg(base_color)
                    .bg(emphasis_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(base_color)
            };
            spans.push(Span::styled(text.to_string(), style));
        }

        last_end = change.end;
    }

    // Remaining text after last change
    if last_end < content.len() {
        spans.push(Span::styled(
            content[last_end..].to_string(),
            Style::default().fg(base_color),
        ));
    }

    spans
}

/// Render entire diff to Vec<Line> for display in ratatui Paragraph
pub fn render_diff<'a>(diff: &str, highlighter: &SyntaxHighlighter) -> Vec<Line<'a>> {
    let parsed = parse_diff_enhanced(diff);

    // Calculate line number width based on max line numbers
    let max_line = parsed
        .iter()
        .filter_map(|l| l.new_line_num.or(l.old_line_num))
        .max()
        .unwrap_or(1);
    let width = max_line.to_string().len().max(3);

    parsed
        .iter()
        .map(|line| render_diff_line(line, highlighter, width))
        .collect()
}
