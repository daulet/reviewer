pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();

    let mut pat_idx = 0usize;
    let mut text_idx = 0usize;
    let mut last_star_idx: Option<usize> = None;
    let mut star_match_text_idx = 0usize;

    while text_idx < text.len() {
        if pat_idx < pattern.len()
            && (pattern[pat_idx] == b'?' || pattern[pat_idx] == text[text_idx])
        {
            pat_idx += 1;
            text_idx += 1;
            continue;
        }

        if pat_idx < pattern.len() && pattern[pat_idx] == b'*' {
            last_star_idx = Some(pat_idx);
            pat_idx += 1;
            star_match_text_idx = text_idx;
            continue;
        }

        if let Some(star_idx) = last_star_idx {
            pat_idx = star_idx + 1;
            star_match_text_idx += 1;
            text_idx = star_match_text_idx;
            continue;
        }

        return false;
    }

    while pat_idx < pattern.len() && pattern[pat_idx] == b'*' {
        pat_idx += 1;
    }

    pat_idx == pattern.len()
}

fn normalize_user_pattern(pattern: &str) -> Option<String> {
    let normalized = pattern
        .trim()
        .trim_start_matches('@')
        .trim()
        .to_ascii_lowercase();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

pub fn normalize_user_patterns(patterns: &[String]) -> Vec<String> {
    let mut normalized = patterns
        .iter()
        .filter_map(|pattern| normalize_user_pattern(pattern))
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

pub fn api_excludable_author_logins(patterns: &[String]) -> Vec<String> {
    normalize_user_patterns(patterns)
        .into_iter()
        .filter(|pattern| {
            !pattern.contains('*')
                && !pattern.contains('?')
                && !pattern.starts_with("app/")
                && !pattern.starts_with("apps/")
        })
        .collect()
}

fn author_is_app_actor(author_kind: Option<&str>) -> bool {
    author_kind
        .map(|kind| matches!(kind.trim().to_ascii_lowercase().as_str(), "app" | "bot"))
        .unwrap_or(false)
}

pub fn author_excluded(author: &str, author_kind: Option<&str>, patterns: &[String]) -> bool {
    let Some(author) = normalize_user_pattern(author) else {
        return false;
    };

    patterns.iter().any(|pattern| {
        let Some(pattern) = normalize_user_pattern(pattern) else {
            return false;
        };

        if let Some(app_pattern) = pattern
            .strip_prefix("apps/")
            .or_else(|| pattern.strip_prefix("app/"))
        {
            return author_is_app_actor(author_kind) && wildcard_match(app_pattern, &author);
        }

        wildcard_match(&pattern, &author)
    })
}

#[cfg(test)]
mod tests {
    use super::{
        api_excludable_author_logins, author_excluded, normalize_user_patterns, wildcard_match,
    };

    #[test]
    fn wildcard_match_supports_star_and_question() {
        assert!(wildcard_match("org/*", "org/reviewer"));
        assert!(wildcard_match("*bot", "dependabot"));
        assert!(wildcard_match("renovate[bo?]", "renovate[bot]"));
        assert!(!wildcard_match("org/*", "other/reviewer"));
        assert!(!wildcard_match("*bot", "alice"));
    }

    #[test]
    fn normalize_user_patterns_trims_at_lowercases_and_dedups() {
        assert_eq!(
            normalize_user_patterns(&[
                " @Apps/* ".to_string(),
                "apps/*".to_string(),
                "".to_string(),
                " Alice ".to_string(),
            ]),
            vec!["alice".to_string(), "apps/*".to_string()]
        );
    }

    #[test]
    fn api_excludable_author_logins_only_returns_exact_non_app_patterns() {
        assert_eq!(
            api_excludable_author_logins(&[
                " @Dependabot ".to_string(),
                "lpu-renovate".to_string(),
                "@apps/*".to_string(),
                "github-*".to_string(),
                "app/copilot".to_string(),
            ]),
            vec!["dependabot".to_string(), "lpu-renovate".to_string()]
        );
    }

    #[test]
    fn author_excluded_matches_exact_users_and_wildcards() {
        let patterns = vec!["@dependabot".to_string(), "lpu-*".to_string()];

        assert!(author_excluded("Dependabot", Some("User"), &patterns));
        assert!(author_excluded("lpu-renovate", Some("Bot"), &patterns));
        assert!(!author_excluded("alice", Some("User"), &patterns));
    }

    #[test]
    fn author_excluded_apps_namespace_only_matches_app_actors() {
        let patterns = vec!["@apps/*".to_string()];

        assert!(author_excluded("lpu-renovate", Some("Bot"), &patterns));
        assert!(author_excluded("github-actions", Some("App"), &patterns));
        assert!(!author_excluded("alice", Some("User"), &patterns));
        assert!(!author_excluded("unknown-bot-name", None, &patterns));
    }

    #[test]
    fn author_excluded_apps_namespace_supports_login_patterns() {
        let patterns = vec!["@apps/lpu-*".to_string()];

        assert!(author_excluded("lpu-renovate", Some("Bot"), &patterns));
        assert!(!author_excluded("github-actions", Some("Bot"), &patterns));
    }
}
