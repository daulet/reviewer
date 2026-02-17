use crate::gh;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

fn is_git_repo(path: &Path) -> bool {
    path.join(".git").is_dir()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredRepo {
    pub path: PathBuf,
    pub name_with_owner: Option<String>,
}

impl DiscoveredRepo {
    fn logical_key(&self) -> String {
        self.name_with_owner
            .clone()
            .unwrap_or_else(|| format!("path:{}", self.path.display()))
    }
}

#[derive(Debug, Clone)]
pub struct RepoScanResult {
    pub discovered_count: usize,
    pub unique_repos: Vec<DiscoveredRepo>,
}

impl RepoScanResult {
    pub fn duplicates_skipped(&self) -> usize {
        self.discovered_count
            .saturating_sub(self.unique_repos.len())
    }
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

fn dedupe_by_key<T, K, P, Fk, Fp>(items: Vec<T>, key_fn: Fk, preference_fn: Fp) -> Vec<T>
where
    K: Ord,
    P: Ord,
    Fk: Fn(&T) -> K,
    Fp: Fn(&T) -> P,
{
    let mut keyed: Vec<(K, P, T)> = items
        .into_iter()
        .map(|item| (key_fn(&item), preference_fn(&item), item))
        .collect();

    keyed.sort_by(|a, b| {
        let by_key = a.0.cmp(&b.0);
        if by_key != std::cmp::Ordering::Equal {
            return by_key;
        }
        a.1.cmp(&b.1)
    });
    keyed.dedup_by(|a, b| a.0 == b.0);

    keyed.into_iter().map(|(_, _, item)| item).collect()
}

pub fn scan_unique_repos(root: &Path, max_depth: usize, exclude: &[String]) -> RepoScanResult {
    let repo_paths = find_repos(root, max_depth, exclude);
    let discovered_count = repo_paths.len();

    let discovered: Vec<DiscoveredRepo> = repo_paths
        .par_iter()
        .map(|path| DiscoveredRepo {
            path: path.clone(),
            name_with_owner: gh::repo_name_with_owner(path),
        })
        .collect();

    let unique_repos = dedupe_by_key(
        discovered,
        |repo| repo.logical_key(),
        |repo| repo.path.to_string_lossy().to_string(),
    );

    RepoScanResult {
        discovered_count,
        unique_repos,
    }
}

#[cfg(test)]
mod tests {
    use super::{dedupe_by_key, DiscoveredRepo};
    use std::path::PathBuf;

    #[test]
    fn dedupe_by_key_keeps_one_entry_per_key() {
        let items = vec![
            ("repo-a".to_string(), "/tmp/z".to_string()),
            ("repo-a".to_string(), "/tmp/a".to_string()),
            ("repo-b".to_string(), "/tmp/b".to_string()),
        ];

        let deduped = dedupe_by_key(items, |item| item.0.clone(), |item| item.1.clone());
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].1, "/tmp/a");
        assert_eq!(deduped[1].1, "/tmp/b");
    }

    #[test]
    fn discovered_repo_logical_key_falls_back_to_path() {
        let repo = DiscoveredRepo {
            path: PathBuf::from("/tmp/project"),
            name_with_owner: None,
        };
        assert_eq!(repo.logical_key(), "path:/tmp/project");
    }
}
