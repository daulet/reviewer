use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[cfg(target_os = "macos")]
use crate::{config, terminal};
#[cfg(target_os = "macos")]
use anyhow::Context;
#[cfg(target_os = "macos")]
use chrono::{DateTime, Utc};
#[cfg(target_os = "macos")]
use serde::Serialize;
#[cfg(target_os = "macos")]
use std::collections::BTreeSet;
#[cfg(target_os = "macos")]
use std::path::Path;
#[cfg(target_os = "macos")]
use std::process::Command;
#[cfg(target_os = "macos")]
use std::thread;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

#[derive(Parser, Debug, Clone)]
pub struct HarnessArgs {
    /// How many launch attempts to run
    #[arg(long, default_value_t = 3)]
    pub runs: u32,

    /// Terminal app name, for example: Ghostty, Terminal, iTerm
    #[arg(long, default_value = "Ghostty")]
    pub terminal_app: String,

    /// Launch mode: auto, new-instance, same-space, new-tab, new-window
    #[arg(long, default_value = "auto")]
    pub terminal_launch_mode: String,

    /// Seconds to keep the launched shell session alive
    #[arg(long, default_value_t = 2)]
    pub hold_seconds: u64,

    /// Seconds to wait after launch before taking the "after" process snapshot
    #[arg(long, default_value_t = 1)]
    pub settle_seconds: u64,

    /// Seconds to wait between runs
    #[arg(long, default_value_t = 1)]
    pub gap_seconds: u64,

    /// Max seconds to wait for the launched command marker file
    #[arg(long, default_value_t = 8)]
    pub verify_timeout_seconds: u64,

    /// Directory to write JSON report to
    #[arg(long)]
    pub output_dir: Option<PathBuf>,

    /// Print what would be launched without launching terminal apps
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
}

pub fn run(args: HarnessArgs) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        run_macos(args)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = args;
        anyhow::bail!("`reviewer harness` currently supports macOS only.");
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug, Serialize)]
struct HarnessReport {
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    terminal_app: String,
    terminal_launch_mode: String,
    dry_run: bool,
    hold_seconds: u64,
    settle_seconds: u64,
    gap_seconds: u64,
    verify_timeout_seconds: u64,
    runs: Vec<RunReport>,
    summary: HarnessSummary,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Serialize)]
struct HarnessSummary {
    requested_runs: u32,
    runs_with_launch_error: usize,
    runs_with_new_pid: usize,
    runs_without_new_pid: usize,
    runs_with_marker_file: usize,
    runs_with_marker_process: usize,
    new_pid_every_run: bool,
    unique_app_pids_seen: Vec<u32>,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Serialize)]
struct RunReport {
    run_index: u32,
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    launch_attempts: u32,
    marker: String,
    command_line: String,
    launch_error: Option<String>,
    marker_file: String,
    marker_file_observed: bool,
    marker_file_observed_at: Option<DateTime<Utc>>,
    marker_file_content: Option<String>,
    marker_pids_after: Vec<u32>,
    session_started: bool,
    before: ProcessSnapshot,
    after: ProcessSnapshot,
    delta: SnapshotDelta,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Serialize)]
struct ProcessSnapshot {
    taken_at: DateTime<Utc>,
    process_count: usize,
    pids: Vec<u32>,
    processes: Vec<ProcessInfo>,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Serialize)]
struct ProcessInfo {
    pid: u32,
    elapsed: String,
    command: String,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Serialize)]
struct SnapshotDelta {
    process_count_delta: i64,
    new_pids: Vec<u32>,
    exited_pids: Vec<u32>,
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct MarkerObservation {
    observed: bool,
    observed_at: Option<DateTime<Utc>>,
    content: Option<String>,
}

#[cfg(target_os = "macos")]
fn run_macos(args: HarnessArgs) -> Result<()> {
    if args.runs == 0 {
        anyhow::bail!("--runs must be greater than 0");
    }

    let launch_mode = terminal::parse_terminal_launch_mode(&args.terminal_launch_mode)?;
    let output_dir = args.output_dir.unwrap_or_else(|| {
        default_output_dir(&args.terminal_app, launch_mode.as_str(), Utc::now())
    });
    std::fs::create_dir_all(&output_dir)
        .with_context(|| format!("Failed to create output directory {}", output_dir.display()))?;

    let started_at = Utc::now();
    let mut runs = Vec::with_capacity(args.runs as usize);
    let mut all_pids = BTreeSet::new();
    let mut runs_with_launch_error = 0usize;
    let mut runs_with_new_pid = 0usize;
    let mut runs_with_marker_file = 0usize;
    let mut runs_with_marker_process = 0usize;

    for run_index in 1..=args.runs {
        let run_started_at = Utc::now();
        let marker = format!(
            "reviewer-harness-{}-{}-{}-{}",
            slug(&args.terminal_app),
            launch_mode.as_str(),
            run_index,
            run_started_at.timestamp_millis()
        );
        let marker_file = output_dir.join(format!("marker-run-{}.txt", run_index));
        let command_line =
            build_harness_command(&marker, run_index, args.hold_seconds, &marker_file);

        let before = capture_snapshot(&args.terminal_app)?;

        let ghostty_retry_mode = args.terminal_app.eq_ignore_ascii_case("ghostty")
            || args.terminal_app.eq_ignore_ascii_case("ghostty.app");
        let should_retry = ghostty_retry_mode
            && matches!(
                launch_mode,
                terminal::TerminalLaunchMode::SameSpace | terminal::TerminalLaunchMode::NewTab
            );
        let max_attempts = if args.dry_run {
            1
        } else if should_retry {
            if matches!(launch_mode, terminal::TerminalLaunchMode::NewTab) {
                3
            } else {
                2
            }
        } else {
            1
        };

        let mut launch_attempts = 0u32;
        let mut launch_error = None;
        let mut marker_observation = MarkerObservation::default();

        for attempt in 1..=max_attempts {
            launch_attempts = attempt;
            let _ = std::fs::remove_file(&marker_file);

            if args.dry_run {
                break;
            }

            launch_error =
                terminal::launch_macos_terminal(&args.terminal_app, &command_line, launch_mode)
                    .err();
            if launch_error.is_some() {
                break;
            }

            marker_observation = wait_for_marker_file(
                &marker_file,
                args.verify_timeout_seconds,
                &marker,
                run_index,
            )?;
            if marker_observation.observed || attempt >= max_attempts {
                break;
            }

            thread::sleep(Duration::from_millis(300));
        }

        if launch_error.is_some() {
            runs_with_launch_error += 1;
        }
        if marker_observation.observed {
            runs_with_marker_file += 1;
        }

        thread::sleep(Duration::from_secs(args.settle_seconds));
        let after = capture_snapshot(&args.terminal_app)?;
        let marker_pids_after = capture_marker_pids(&marker)?;
        if !marker_pids_after.is_empty() {
            runs_with_marker_process += 1;
        }
        let delta = diff_snapshots(&before, &after);
        if !delta.new_pids.is_empty() {
            runs_with_new_pid += 1;
        }
        for pid in &after.pids {
            all_pids.insert(*pid);
        }

        runs.push(RunReport {
            run_index,
            started_at: run_started_at,
            finished_at: Utc::now(),
            launch_attempts,
            marker,
            command_line,
            launch_error: launch_error.map(|err| err.to_string()),
            marker_file: marker_file.display().to_string(),
            marker_file_observed: marker_observation.observed,
            marker_file_observed_at: marker_observation.observed_at,
            marker_file_content: marker_observation.content,
            session_started: marker_observation.observed || !marker_pids_after.is_empty(),
            marker_pids_after,
            before,
            after,
            delta,
        });

        if run_index < args.runs {
            thread::sleep(Duration::from_secs(args.gap_seconds));
        }
    }

    let summary = HarnessSummary {
        requested_runs: args.runs,
        runs_with_launch_error,
        runs_with_new_pid,
        runs_without_new_pid: args.runs as usize - runs_with_new_pid,
        runs_with_marker_file,
        runs_with_marker_process,
        new_pid_every_run: runs_with_new_pid == args.runs as usize,
        unique_app_pids_seen: all_pids.into_iter().collect(),
    };

    let finished_at = Utc::now();
    let report = HarnessReport {
        started_at,
        finished_at,
        terminal_app: args.terminal_app.clone(),
        terminal_launch_mode: launch_mode.as_str().to_string(),
        dry_run: args.dry_run,
        hold_seconds: args.hold_seconds,
        settle_seconds: args.settle_seconds,
        gap_seconds: args.gap_seconds,
        verify_timeout_seconds: args.verify_timeout_seconds,
        runs,
        summary,
    };

    let report_path = output_dir.join("report.json");
    let report_json = serde_json::to_string_pretty(&report)?;
    std::fs::write(&report_path, report_json)
        .with_context(|| format!("Failed to write report to {}", report_path.display()))?;

    println!("Harness report: {}", report_path.display());
    println!(
        "Mode={} app={} dry_run={}",
        launch_mode.as_str(),
        args.terminal_app,
        args.dry_run
    );
    println!(
        "Runs with new app PID(s): {}/{}",
        report.summary.runs_with_new_pid, report.summary.requested_runs
    );
    println!(
        "Runs without new app PID(s): {}/{}",
        report.summary.runs_without_new_pid, report.summary.requested_runs
    );
    println!(
        "Runs with marker file (command executed): {}/{}",
        report.summary.runs_with_marker_file, report.summary.requested_runs
    );
    println!(
        "Runs with marker shell process: {}/{}",
        report.summary.runs_with_marker_process, report.summary.requested_runs
    );
    println!(
        "New PID every run: {}",
        if report.summary.new_pid_every_run {
            "yes"
        } else {
            "no"
        }
    );
    if report.summary.runs_with_launch_error > 0 {
        println!(
            "Runs with launch errors: {}",
            report.summary.runs_with_launch_error
        );
    }
    if !args.dry_run {
        let missing_markers =
            report.summary.requested_runs as usize - report.summary.runs_with_marker_file;
        if report.summary.runs_with_launch_error > 0 || missing_markers > 0 {
            anyhow::bail!(
                "Harness verification failed: launch_errors={}, missing_command_markers={}, report={}",
                report.summary.runs_with_launch_error,
                missing_markers,
                report_path.display()
            );
        }
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn default_output_dir(app: &str, mode: &str, now: DateTime<Utc>) -> PathBuf {
    let stamp = now.format("%Y%m%dT%H%M%SZ");
    config::config_dir()
        .join("launch_harness")
        .join(format!("{}-{}-{}", stamp, slug(app), mode))
}

#[cfg(target_os = "macos")]
fn build_harness_command(
    marker: &str,
    run_index: u32,
    hold_seconds: u64,
    marker_file: &Path,
) -> String {
    let marker_file_escaped = unix_shell_escape(&marker_file.display().to_string());
    format!(
        "printf '%s\\n' 'HARNESS_MARKER={marker}' 'HARNESS_RUN={run_index}' > {marker_file}; date -u '+%Y-%m-%dT%H:%M:%SZ' >> {marker_file}; sleep {hold_seconds}",
        marker_file = marker_file_escaped
    )
}

#[cfg(target_os = "macos")]
fn wait_for_marker_file(
    path: &Path,
    timeout_seconds: u64,
    expected_marker: &str,
    expected_run_index: u32,
) -> Result<MarkerObservation> {
    let timeout = Duration::from_secs(timeout_seconds);
    let started = Instant::now();
    let expected_marker_line = format!("HARNESS_MARKER={expected_marker}");
    let expected_run_line = format!("HARNESS_RUN={expected_run_index}");
    let mut last_content: Option<String> = None;
    loop {
        if let Ok(content) = std::fs::read_to_string(path) {
            let valid =
                content.contains(&expected_marker_line) && content.contains(&expected_run_line);
            if valid {
                return Ok(MarkerObservation {
                    observed: true,
                    observed_at: Some(Utc::now()),
                    content: Some(content),
                });
            }
            last_content = Some(content);
        }
        if started.elapsed() >= timeout {
            return Ok(MarkerObservation {
                observed: false,
                observed_at: None,
                content: last_content,
            });
        }
        thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(target_os = "macos")]
fn capture_snapshot(app: &str) -> Result<ProcessSnapshot> {
    let output = Command::new("ps")
        .arg("-ax")
        .output()
        .context("Failed to run ps for process snapshot")?;
    if !output.status.success() {
        anyhow::bail!("ps returned non-zero status");
    }

    let candidates = app_name_candidates(app);
    let mut processes = Vec::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let Some(info) = parse_process_line(line) else {
            continue;
        };
        if process_matches_app(&info.command, &candidates) {
            processes.push(info);
        }
    }

    processes.sort_by_key(|p| p.pid);
    let pids = processes.iter().map(|p| p.pid).collect::<Vec<_>>();

    Ok(ProcessSnapshot {
        taken_at: Utc::now(),
        process_count: processes.len(),
        pids,
        processes,
    })
}

#[cfg(target_os = "macos")]
fn capture_marker_pids(marker: &str) -> Result<Vec<u32>> {
    let output = Command::new("ps")
        .arg("-ax")
        .output()
        .context("Failed to run ps for marker snapshot")?;
    if !output.status.success() {
        anyhow::bail!("ps returned non-zero status");
    }

    let mut pids = Vec::new();
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if !line.contains(marker) {
            continue;
        }
        let Some(pid) = line
            .split_whitespace()
            .next()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        pids.push(pid);
    }

    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

#[cfg(target_os = "macos")]
fn parse_process_line(line: &str) -> Option<ProcessInfo> {
    let mut parts = line.split_whitespace();
    let pid = parts.next()?.parse::<u32>().ok()?;
    let _tty = parts.next()?;
    let elapsed = parts.next()?.to_string();
    let command = parts.collect::<Vec<_>>().join(" ");
    if command.is_empty() {
        return None;
    }
    Some(ProcessInfo {
        pid,
        elapsed,
        command,
    })
}

#[cfg(target_os = "macos")]
fn app_name_candidates(app: &str) -> Vec<String> {
    let mut names = Vec::new();
    let trimmed = app.trim();
    let without_app_suffix = trimmed.trim_end_matches(".app");

    for value in [trimmed, without_app_suffix] {
        if value.is_empty() {
            continue;
        }
        names.push(value.to_ascii_lowercase());

        let path = Path::new(value);
        if let Some(file_name) = path.file_name().and_then(|v| v.to_str()) {
            names.push(file_name.to_ascii_lowercase());
        }
        if let Some(stem) = path.file_stem().and_then(|v| v.to_str()) {
            names.push(stem.to_ascii_lowercase());
        }
    }

    names.sort();
    names.dedup();
    names
}

#[cfg(target_os = "macos")]
fn process_matches_app(command: &str, candidates: &[String]) -> bool {
    let lowered = command.to_ascii_lowercase();
    let basename = lowered.rsplit('/').next().unwrap_or(lowered.as_str());
    candidates.iter().any(|candidate| {
        basename == candidate || basename.strip_suffix(".app") == Some(candidate.as_str())
    })
}

#[cfg(target_os = "macos")]
fn diff_snapshots(before: &ProcessSnapshot, after: &ProcessSnapshot) -> SnapshotDelta {
    let before_pids: BTreeSet<u32> = before.pids.iter().copied().collect();
    let after_pids: BTreeSet<u32> = after.pids.iter().copied().collect();

    SnapshotDelta {
        process_count_delta: after.process_count as i64 - before.process_count as i64,
        new_pids: after_pids.difference(&before_pids).copied().collect(),
        exited_pids: before_pids.difference(&after_pids).copied().collect(),
    }
}

#[cfg(target_os = "macos")]
fn unix_shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
fn slug(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}
