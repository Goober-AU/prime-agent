//! Port of packages/coding-agent/src/core/session-resolver.ts

use std::collections::BTreeSet;

use crate::core::session_id::{matches_saved_session_selector, normalize_session_id};
use crate::core::session_manager::{SessionInfo, SessionManager};

/// `ResolvedSession` - the `type` discriminant is preserved as an enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedSession {
    Path { path: String },
    Local { path: String },
    Global { path: String, cwd: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSelectorError {
    pub message: String,
    pub selector: String,
}

impl std::fmt::Display for SessionSelectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SessionSelectorError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSelectorNotFoundError {
    pub error: SessionSelectorError,
    /// `suggestion?`
    pub suggestion: Option<String>,
}

impl std::fmt::Display for SessionSelectorNotFoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.error.message)
    }
}

impl std::error::Error for SessionSelectorNotFoundError {}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSelectorAmbiguousError {
    pub error: SessionSelectorError,
    pub matches: Vec<SessionInfo>,
}

impl std::fmt::Display for SessionSelectorAmbiguousError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.error.message)
    }
}

impl std::error::Error for SessionSelectorAmbiguousError {}

/// `SessionSelectorError` subtypes are reported as one error type so callers can
/// match on the variant, exactly like `instanceof` checks in the TypeScript.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolveSessionError {
    NotFound(SessionSelectorNotFoundError),
    Ambiguous(SessionSelectorAmbiguousError),
    Other(SessionSelectorError),
}

impl std::fmt::Display for ResolveSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveSessionError::NotFound(error) => f.write_str(&error.error.message),
            ResolveSessionError::Ambiguous(error) => f.write_str(&error.error.message),
            ResolveSessionError::Other(error) => f.write_str(&error.message),
        }
    }
}

impl std::error::Error for ResolveSessionError {}

impl ResolveSessionError {
    pub fn selector(&self) -> &str {
        match self {
            ResolveSessionError::NotFound(error) => &error.error.selector,
            ResolveSessionError::Ambiguous(error) => &error.error.selector,
            ResolveSessionError::Other(error) => &error.selector,
        }
    }
}

pub fn looks_like_session_path(selector: &str) -> bool {
    selector.contains('/') || selector.contains('\\') || selector.ends_with(".jsonl")
}

pub async fn resolve_session_path(
    selector: &str,
    cwd: &str,
    session_dir: Option<&str>,
) -> Result<ResolvedSession, ResolveSessionError> {
    if looks_like_session_path(selector) {
        return Ok(ResolvedSession::Path {
            path: selector.to_string(),
        });
    }

    let local_sessions = SessionManager::list(cwd, session_dir, None).await;
    if let Some(matched) = resolve_exact_match(selector, &local_sessions)? {
        return Ok(ResolvedSession::Local { path: matched.path });
    }

    let all_sessions = SessionManager::list_all(None, session_dir).await;
    if let Some(matched) = resolve_exact_match(selector, &all_sessions)? {
        return Ok(ResolvedSession::Global {
            path: matched.path,
            cwd: matched.cwd,
        });
    }

    if let Some(matched) = resolve_partial_match(selector, &local_sessions)? {
        return Ok(ResolvedSession::Local { path: matched.path });
    }

    if let Some(matched) = resolve_partial_match(selector, &all_sessions)? {
        return Ok(ResolvedSession::Global {
            path: matched.path,
            cwd: matched.cwd,
        });
    }

    let mut combined = local_sessions.clone();
    combined.extend(all_sessions.iter().cloned());
    let suggestion = find_closest_session_id(selector, &combined);
    Err(ResolveSessionError::NotFound(
        SessionSelectorNotFoundError {
            error: SessionSelectorError {
                message: format!("No session found matching '{selector}'"),
                selector: selector.to_string(),
            },
            suggestion,
        },
    ))
}

pub fn find_closest_session_id(selector: &str, sessions: &[SessionInfo]) -> Option<String> {
    let normalized_selector = normalize_session_id(selector);
    if normalized_selector.chars().count() < 4 {
        return None;
    }

    let mut unique_ids: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for session in sessions {
        if seen.insert(session.id.clone()) {
            unique_ids.push(session.id.clone());
        }
    }

    let mut closest: Option<(String, usize)> = None;
    let mut tied = false;

    for id in unique_ids {
        let normalized_id = normalize_session_id(&id);
        let length = normalized_selector
            .chars()
            .count()
            .min(normalized_id.chars().count());
        let head: String = normalized_id.chars().take(length).collect();
        let tail: String = {
            let chars: Vec<char> = normalized_id.chars().collect();
            chars[chars.len().saturating_sub(length)..].iter().collect()
        };
        let distance = edit_distance(&normalized_selector, &head)
            .min(edit_distance(&normalized_selector, &tail));
        match &closest {
            None => {
                closest = Some((id, distance));
                tied = false;
            }
            Some((_, best)) if distance < *best => {
                closest = Some((id, distance));
                tied = false;
            }
            Some((_, best)) if distance == *best => {
                tied = true;
            }
            Some(_) => {}
        }
    }

    let maximum_distance = 1.max(normalized_selector.chars().count() / 5);
    match closest {
        Some((id, distance)) if !tied && distance <= maximum_distance => Some(id),
        _ => None,
    }
}

fn resolve_exact_match(
    selector: &str,
    sessions: &[SessionInfo],
) -> Result<Option<SessionInfo>, ResolveSessionError> {
    let normalized_selector = normalize_session_id(selector);
    let matches: Vec<SessionInfo> = sessions
        .iter()
        .filter(|session| normalize_session_id(&session.id) == normalized_selector)
        .cloned()
        .collect();
    resolve_unique_match(selector, matches)
}

fn resolve_partial_match(
    selector: &str,
    sessions: &[SessionInfo],
) -> Result<Option<SessionInfo>, ResolveSessionError> {
    let matches: Vec<SessionInfo> = sessions
        .iter()
        .filter(|session| matches_saved_session_selector(&session.id, selector))
        .cloned()
        .collect();
    resolve_unique_match(selector, matches)
}

fn resolve_unique_match(
    selector: &str,
    matches: Vec<SessionInfo>,
) -> Result<Option<SessionInfo>, ResolveSessionError> {
    if matches.len() > 1 {
        let joined = matches
            .iter()
            .map(|session| {
                if let Some(name) = session.name.as_ref() {
                    format!("{} ({name})", session.id)
                } else {
                    session.id.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ResolveSessionError::Ambiguous(
            SessionSelectorAmbiguousError {
                error: SessionSelectorError {
                    message: format!("Ambiguous saved session \"{selector}\": matches {joined}"),
                    selector: selector.to_string(),
                },
                matches,
            },
        ));
    }
    Ok(matches.into_iter().next())
}

fn edit_distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for left_index in 1..=left.len() {
        let mut diagonal = previous[0];
        previous[0] = left_index;
        for right_index in 1..=right.len() {
            let above = previous[right_index];
            previous[right_index] = (previous[right_index] + 1)
                .min(previous[right_index - 1] + 1)
                .min(diagonal + usize::from(left[left_index - 1] != right[right_index - 1]));
            diagonal = above;
        }
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::session_manager::SessionInfo;

    fn info(id: &str, name: Option<&str>) -> SessionInfo {
        SessionInfo {
            path: format!("/sessions/{id}.jsonl"),
            id: id.to_string(),
            cwd: "/work".to_string(),
            name: name.map(str::to_string),
            state: None,
            parent_session_path: None,
            rlm_depth: 0,
            created: 0.0,
            modified: 0.0,
            message_count: 0,
            first_message: String::new(),
            all_messages_text: String::new(),
            agent_status: None,
            usage: None,
        }
    }

    #[test]
    fn detects_path_like_selectors() {
        assert!(looks_like_session_path("/tmp/a.jsonl"));
        assert!(looks_like_session_path("dir\\a"));
        assert!(looks_like_session_path("a.jsonl"));
        assert!(!looks_like_session_path("01924f7a"));
    }

    #[test]
    fn edit_distance_matches_the_typescript_algorithm() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("abc", "abd"), 1);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn finds_closest_ids_only_when_unambiguous_and_close() {
        let sessions = vec![info("01924f7a1234", None), info("ffffffff9999", None)];
        assert_eq!(
            find_closest_session_id("01924f7a1234", &sessions).as_deref(),
            Some("01924f7a1234")
        );
        assert_eq!(find_closest_session_id("abc", &sessions), None);
        let tied = vec![info("aaaa", None), info("bbbb", None)];
        assert_eq!(
            find_closest_session_id("aaaa", &tied),
            Some("aaaa".to_string())
        );
    }

    #[test]
    fn ambiguous_partial_matches_report_both_names() {
        let sessions = vec![info("abcd1", Some("one")), info("abcd2", None)];
        let error = resolve_partial_match("abcd", &sessions).unwrap_err();
        match error {
            ResolveSessionError::Ambiguous(ambiguous) => {
                assert_eq!(
                    ambiguous.error.message,
                    "Ambiguous saved session \"abcd\": matches abcd1 (one), abcd2"
                );
                assert_eq!(ambiguous.matches.len(), 2);
            }
            other => panic!("expected ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn exact_match_prefers_normalized_ids() {
        let sessions = vec![info("01924F7A-1234", None)];
        let matched = resolve_exact_match("01924f7a1234", &sessions)
            .unwrap()
            .unwrap();
        assert_eq!(matched.id, "01924F7A-1234");
    }

    #[tokio::test]
    async fn path_like_selectors_skip_the_session_list() {
        let resolved = resolve_session_path("./x.jsonl", "/work", None)
            .await
            .unwrap();
        assert_eq!(
            resolved,
            ResolvedSession::Path {
                path: "./x.jsonl".to_string()
            }
        );
    }
}
