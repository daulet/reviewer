use std::path::PathBuf;
use walkdir::WalkDir;

fn is_git_repo(path: &PathBuf) -> bool {
    path.join(".git").is_dir()
}

pub fn find_repos(root: &PathBuf, max_depth: usize) -> Vec<PathBuf> {
    let mut repos = Vec::new();

    for entry in WalkDir::new(root)
        .max_depth(max_depth)
        .into_iter()
        .filter_entry(|e| {
            !e.file_name()
                .to_str()
                .map(|s| s.starts_with('.'))
                .unwrap_or(false)
                || e.depth() == 0
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
