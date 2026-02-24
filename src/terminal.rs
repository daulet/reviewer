#[cfg(target_os = "macos")]
use anyhow::{Context, Result};
#[cfg(target_os = "macos")]
use std::process::Command;

#[cfg(target_os = "macos")]
pub const TERMINAL_LAUNCH_MODE_VALUES: &[&str] = &[
    "auto",
    "new-instance",
    "same-space",
    "new-tab",
    "new-window",
];

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalLaunchMode {
    Auto,
    NewInstance,
    SameSpace,
    NewTab,
    NewWindow,
}

#[cfg(target_os = "macos")]
impl TerminalLaunchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::NewInstance => "new-instance",
            Self::SameSpace => "same-space",
            Self::NewTab => "new-tab",
            Self::NewWindow => "new-window",
        }
    }
}

#[cfg(target_os = "macos")]
pub fn parse_terminal_launch_mode(value: &str) -> Result<TerminalLaunchMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => Ok(TerminalLaunchMode::Auto),
        "new-instance" => Ok(TerminalLaunchMode::NewInstance),
        "same-space" => Ok(TerminalLaunchMode::SameSpace),
        "new-tab" => Ok(TerminalLaunchMode::NewTab),
        "new-window" => Ok(TerminalLaunchMode::NewWindow),
        other => anyhow::bail!(
            "Invalid terminal launch mode '{}'. Expected one of: {}",
            other,
            TERMINAL_LAUNCH_MODE_VALUES.join(", ")
        ),
    }
}

#[cfg(target_os = "macos")]
fn launch_macos_terminal_applescript(app: &str, command_line: &str) -> Result<()> {
    let escaped_command = command_line.replace('"', "\\\"");
    let script = format!(
        r#"tell application "{app}"
            activate
            do script "{command}"
        end tell"#,
        app = app,
        command = escaped_command
    );
    let output = Command::new("osascript")
        .args(["-e", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .with_context(|| format!("Failed to launch {}", app))?;
    if !output.status.success() {
        anyhow::bail!(
            "Failed to launch {}: {}",
            app,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn escape_applescript_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(target_os = "macos")]
fn launch_macos_ghostty_new_tab(app: &str, command_line: &str) -> Result<()> {
    // Ensure Ghostty is running/active before sending keybindings.
    let activate_output = Command::new("open")
        .args(["-a", app])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .with_context(|| format!("Failed to activate {}", app))?;
    if !activate_output.status.success() {
        anyhow::bail!(
            "Failed to activate {}: {}",
            app,
            String::from_utf8_lossy(&activate_output.stderr).trim()
        );
    }

    let escaped_command = escape_applescript_string(command_line);
    let script_app = escape_applescript_string(app.trim().trim_end_matches(".app").trim());
    let script = format!(
        r#"set launchCmd to "{command}"
set oldClipboard to ""
try
    set oldClipboard to (the clipboard as text)
end try
set the clipboard to launchCmd
tell application "{app_name}" to activate
delay 0.25
tell application "System Events"
    if exists process "{app_name}" then
        tell process "{app_name}"
            set frontmost to true
        end tell
    end if
end tell
delay 0.15
tell application "System Events"
    keystroke "t" using command down
    delay 0.25
    keystroke "v" using command down
    delay 0.1
    key code 36
end tell
delay 0.2
try
    set the clipboard to oldClipboard
end try"#,
        app_name = script_app,
        command = escaped_command
    );

    let output = Command::new("osascript")
        .args(["-e", &script])
        .output()
        .context("Failed to send Ghostty new-tab AppleScript")?;
    if !output.status.success() {
        anyhow::bail!(
            "Ghostty new-tab automation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn launch_macos_terminal_terminal_new_tab(command_line: &str) -> Result<()> {
    let escaped_command = command_line.replace('"', "\\\"");
    let script = format!(
        r#"tell application "Terminal"
            activate
            if (count of windows) is 0 then
                do script "{command}"
            else
                do script "{command}" in front window
            end if
        end tell"#,
        command = escaped_command
    );
    Command::new("osascript")
        .args(["-e", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Failed to launch Terminal new tab")?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn launch_macos_terminal_open(
    app: &str,
    command_line: &str,
    new_instance: bool,
    extra_args: &[&str],
) -> Result<()> {
    let app_flag = if new_instance { "-na" } else { "-a" };
    let mut args: Vec<&str> = Vec::with_capacity(extra_args.len() + 7);
    args.push(app_flag);
    args.push(app);
    args.push("--args");
    args.extend(extra_args.iter().copied());
    args.push("-e");
    args.push("bash");
    args.push("-lc");
    args.push(command_line);

    let output = Command::new("open")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .with_context(|| format!("Failed to launch {}", app))?;
    if !output.status.success() {
        anyhow::bail!(
            "Failed to launch {}: {}",
            app,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    // Best-effort activation for third-party terminals (Ghostty, iTerm, etc.).
    // Some apps start command execution only after the window becomes active.
    let script_app = app
        .trim()
        .trim_end_matches(".app")
        .trim()
        .replace('"', "\\\"");
    let _ = Command::new("osascript")
        .args([
            "-e",
            "delay 0.1",
            "-e",
            &format!("tell application \"{}\" to activate", script_app),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    Ok(())
}

#[cfg(target_os = "macos")]
pub fn launch_macos_terminal(
    app: &str,
    command_line: &str,
    mode: TerminalLaunchMode,
) -> Result<()> {
    let terminal_app = app.trim();
    if terminal_app.is_empty() {
        anyhow::bail!("Terminal app cannot be empty");
    }

    let is_terminal = terminal_app.eq_ignore_ascii_case("terminal");
    let is_ghostty = terminal_app.eq_ignore_ascii_case("ghostty")
        || terminal_app.eq_ignore_ascii_case("ghostty.app");

    match mode {
        TerminalLaunchMode::Auto => {
            if is_terminal {
                launch_macos_terminal_applescript("Terminal", command_line)
            } else if is_ghostty {
                // Ghostty on macOS does not reliably execute command args via `open -a` when already running.
                // Use `-na` for a deterministic launch.
                launch_macos_terminal_open(terminal_app, command_line, true, &[])
            } else {
                // Reuse a running app instance to avoid forcing a brand new process.
                launch_macos_terminal_open(terminal_app, command_line, false, &[])
            }
        }
        TerminalLaunchMode::NewInstance => {
            launch_macos_terminal_open(terminal_app, command_line, true, &[])
        }
        TerminalLaunchMode::SameSpace => {
            if is_ghostty {
                launch_macos_ghostty_new_tab(terminal_app, command_line)
            } else {
                launch_macos_terminal_open(terminal_app, command_line, false, &[])
            }
        }
        TerminalLaunchMode::NewTab => {
            if is_terminal {
                launch_macos_terminal_terminal_new_tab(command_line)
            } else if is_ghostty {
                launch_macos_ghostty_new_tab(terminal_app, command_line)
            } else {
                launch_macos_terminal_open(terminal_app, command_line, false, &[])
            }
        }
        TerminalLaunchMode::NewWindow => {
            if is_terminal {
                launch_macos_terminal_applescript("Terminal", command_line)
            } else if is_ghostty {
                // Ghostty's +new-window action is GTK-only; use a new app instance on macOS.
                launch_macos_terminal_open(terminal_app, command_line, true, &[])
            } else {
                launch_macos_terminal_open(terminal_app, command_line, false, &[])
            }
        }
    }
}
