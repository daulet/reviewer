use std::path::{Path, PathBuf};
use walkdir::WalkDir;

fn is_git_repo(path: &Path) -> bool {
    path.join(".git").is_dir()
}

/// Find git repositories under root, excluding specified directories.
/// Exclusions are relative paths from root (e.g., "archived", "vendor/old").
pub fn find_repos(root: &Path, max_depth: usize, exclude: &[String]) -> Vec<PathBuf> {
    // Convert exclusions to absolute paths for comparison
    let excluded_paths: Vec<PathBuf> = exclude.iter().map(|e| root.join(e)).collect();

    let mut repos = Vec::new();

    for entry in WalkDir::new(root)
        .max_depth(max_depth)
        .into_iter()
        .filter_entry(|e| {
            let path = e.path();

            // Always allow root
            if e.depth() == 0 {
                return true;
            }

            // Skip hidden directories
            if e.file_name()
                .to_str()
                .map(|s| s.starts_with('.'))
                .unwrap_or(false)
            {
                return false;
            }

            // Skip excluded directories
            !excluded_paths.iter().any(|ex| path.starts_with(ex))
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path().to_path_buf();
        if path.is_dir() && is_git_repo(&path) {
            repos.push(path);
        }
    }

    repos.sort();
    repos
}
