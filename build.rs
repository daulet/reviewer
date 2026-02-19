use std::process::Command;

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8(output.stdout).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/packed-refs");
    println!("cargo:rerun-if-changed=.git/refs");

    let exact_tag = git_output(&["describe", "--tags", "--exact-match"]);
    let latest_tag = git_output(&["describe", "--tags", "--abbrev=0"]);
    let commit = git_output(&["rev-parse", "--short=12", "HEAD"]);

    let version = match (exact_tag, latest_tag, commit) {
        (Some(tag), _, _) => tag,
        (None, Some(tag), Some(sha)) => format!("{tag}+{sha}"),
        (None, Some(tag), None) => tag,
        (None, None, Some(sha)) => sha,
        (None, None, None) => "unknown".to_string(),
    };

    println!("cargo:rustc-env=REVIEWER_VERSION_STRING={version}");
}
