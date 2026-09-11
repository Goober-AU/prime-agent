//! Port of packages/coding-agent/src/cli/daemon-stop-confirm.ts
//!
//! Shared confirmation for stopping a running daemon that has live sessions.
//!
//! Both `prime-agent update --self` and interactive startup (when taking over a
//! stale-version daemon) need to ask before discarding busy sessions. They keep
//! the same busy-session semantics here and only vary the wording via `copy`.
//!
//! Kept out of daemon-launch.ts so the early fire-and-forget launch path stays
//! light on imports (no readline/chalk); this module is only reached on the
//! interactive, post-startup paths.

use super::daemon_launch::{is_session_busy, RunningDaemonProbe};

/// Prompt for a yes/no answer at a TTY. Empty/anything-but-yes resolves false (default No).
pub fn prompt_yes_no(message: &str, read_line: &dyn Fn(&str) -> String) -> bool {
    let answer = read_line(&format!("{} [y/N] ", message));
    let normalized = answer.trim().to_lowercase();
    normalized == "y" || normalized == "yes"
}

pub struct Pluralized {
    pub noun: &'static str,
    pub pronoun: &'static str,
}

pub fn pluralize_sessions(count: i64) -> Pluralized {
    if count == 1 {
        Pluralized { noun: "session", pronoun: "it" }
    } else {
        Pluralized { noun: "sessions", pronoun: "them" }
    }
}

pub struct DaemonSessionLossCopy<'a> {
    /// Full sentence describing the busy sessions and what stopping the daemon does.
    pub busy_detail: &'a dyn Fn(i64) -> String,
    /// Full sentence for the reachable-but-unlistable case (work may be lost).
    pub unlistable_detail: &'a str,
    /// Question appended after the detail when prompting at a TTY (before " [y/N]").
    pub question: &'a str,
    /// Remediation appended after the detail when not at a TTY.
    pub non_tty_hint: &'a str,
}

pub struct ConfirmOptions<'a> {
    pub force: bool,
    pub copy: DaemonSessionLossCopy<'a>,
}

pub struct ConfirmIo<'a> {
    /// `process.stdin.isTTY`
    pub stdin_is_tty: Option<bool>,
    pub error: &'a dyn Fn(&str),
    pub read_line: &'a dyn Fn(&str) -> String,
}

/// Returns true when it is safe to proceed with stopping the daemon: it is not
/// reachable, `force` is set, no sessions are busy, or the user confirmed at a
/// TTY. Returns false to abort (busy/unlistable and either declined or non-TTY).
/// Only busy sessions (streaming, compacting, running bash, or pending messages)
/// require confirmation; idle loaded sessions are restored from disk.
pub fn confirm_daemon_session_loss(
    probe: &RunningDaemonProbe,
    options: ConfirmOptions<'_>,
    io: &ConfirmIo<'_>,
) -> bool {
    let force = options.force;
    let copy = options.copy;
    if !probe.reachable || force {
        return true;
    }
    let detail: String;
    match &probe.active_sessions {
        None => {
            // Reachable but couldn't list sessions: assume work may be lost.
            detail = copy.unlistable_detail.to_string();
        }
        Some(active_sessions) => {
            let busy_session_count =
                active_sessions.iter().filter(|summary| is_session_busy(summary)).count() as i64
                    + probe.busy_client_owned_session_count.unwrap_or(0);
            if busy_session_count == 0 {
                return true;
            }
            detail = (copy.busy_detail)(busy_session_count);
        }
    }
    if io.stdin_is_tty != Some(true) {
        (io.error)(&format!(
            "\u{1b}[31m{} {}\u{1b}[39m",
            detail, copy.non_tty_hint
        ));
        return false;
    }
    prompt_yes_no(&format!("{} {}", detail, copy.question), io.read_line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::daemon_launch::SessionSummaryStub;
    use std::cell::RefCell;

    fn summary(is_session_active: bool, has_running_rlm_children: Option<bool>) -> SessionSummaryStub {
        SessionSummaryStub { is_session_active, has_running_rlm_children }
    }

    fn copy() -> DaemonSessionLossCopy<'static> {
        let busy_detail: &'static dyn Fn(i64) -> String =
            &(|count: i64| format!("{} busy session(s) will be discarded.", count));
        DaemonSessionLossCopy {
            busy_detail,
            unlistable_detail: "The daemon is busy.",
            question: "Stop it anyway?",
            non_tty_hint: "Use --force.",
        }
    }

    #[test]
    fn prompt_yes_no_accepts_only_y_and_yes() {
        assert!(prompt_yes_no("q", &|_| "y".to_string()));
        assert!(prompt_yes_no("q", &|_| " YES \n".to_string()));
        assert!(!prompt_yes_no("q", &|_| "".to_string()));
        assert!(!prompt_yes_no("q", &|_| "yeah".to_string()));
        assert!(!prompt_yes_no("q", &|_| "n".to_string()));
    }

    #[test]
    fn not_reachable_or_forced_is_always_safe() {
        let errors = RefCell::new(Vec::new());
        let error_fn = |message: &str| errors.borrow_mut().push(message.to_string());
        let io = ConfirmIo { stdin_is_tty: Some(false), error: &error_fn, read_line: &|_: &str| "y".to_string() };
        let probe = RunningDaemonProbe { reachable: false, active_sessions: None, busy_client_owned_session_count: None };
        assert!(confirm_daemon_session_loss(
            &probe,
            ConfirmOptions { force: false, copy: copy() },
            &io
        ));

        let probe = RunningDaemonProbe {
            reachable: true,
            active_sessions: Some(vec![summary(true, None)]),
            busy_client_owned_session_count: None,
        };
        assert!(confirm_daemon_session_loss(
            &probe,
            ConfirmOptions { force: true, copy: copy() },
            &io
        ));
    }

    #[test]
    fn idle_sessions_do_not_need_confirmation() {
        let errors = RefCell::new(Vec::new());
        let error_fn = |message: &str| errors.borrow_mut().push(message.to_string());
        let io = ConfirmIo { stdin_is_tty: Some(false), error: &error_fn, read_line: &|_: &str| "y".to_string() };
        let probe = RunningDaemonProbe {
            reachable: true,
            active_sessions: Some(vec![summary(false, None), summary(false, Some(false))]),
            busy_client_owned_session_count: Some(0),
        };
        assert!(confirm_daemon_session_loss(
            &probe,
            ConfirmOptions { force: false, copy: copy() },
            &io
        ));
        assert!(errors.borrow().is_empty());
    }

    #[test]
    fn busy_sessions_require_confirmation_and_the_count_includes_client_owned() {
        let errors = RefCell::new(Vec::new());
        let error_fn = |message: &str| errors.borrow_mut().push(message.to_string());
        let prompt_seen: RefCell<Option<String>> = RefCell::new(None);
        let read_line = |prompt: &str| {
            *prompt_seen.borrow_mut() = Some(prompt.to_string());
            "yes".to_string()
        };
        let io = ConfirmIo { stdin_is_tty: Some(true), error: &error_fn, read_line: &read_line };
        let probe = RunningDaemonProbe {
            reachable: true,
            active_sessions: Some(vec![summary(true, None), summary(false, None)]),
            busy_client_owned_session_count: Some(2),
        };
        assert!(confirm_daemon_session_loss(
            &probe,
            ConfirmOptions { force: false, copy: copy() },
            &io
        ));
        assert_eq!(
            prompt_seen.borrow().as_deref(),
            Some("3 busy session(s) will be discarded. Stop it anyway? [y/N] ")
        );
    }

    #[test]
    fn running_rlm_children_count_as_busy() {
        let errors = RefCell::new(Vec::new());
        let error_fn = |message: &str| errors.borrow_mut().push(message.to_string());
        let prompt_seen: RefCell<Option<String>> = RefCell::new(None);
        let read_line = |prompt: &str| {
            *prompt_seen.borrow_mut() = Some(prompt.to_string());
            "no".to_string()
        };
        let io = ConfirmIo { stdin_is_tty: Some(true), error: &error_fn, read_line: &read_line };
        let probe = RunningDaemonProbe {
            reachable: true,
            active_sessions: Some(vec![summary(false, Some(true))]),
            busy_client_owned_session_count: None,
        };
        assert!(!confirm_daemon_session_loss(
            &probe,
            ConfirmOptions { force: false, copy: copy() },
            &io
        ));
        assert!(prompt_seen.borrow().is_some());
    }

    #[test]
    fn unlistable_sessions_ask_at_a_tty_and_error_off_a_tty() {
        let errors = RefCell::new(Vec::new());
        let error_fn = |message: &str| errors.borrow_mut().push(message.to_string());
        let prompt_seen: RefCell<Option<String>> = RefCell::new(None);
        let read_line = |prompt: &str| {
            *prompt_seen.borrow_mut() = Some(prompt.to_string());
            "y".to_string()
        };
        let io = ConfirmIo { stdin_is_tty: Some(true), error: &error_fn, read_line: &read_line };
        let probe =
            RunningDaemonProbe { reachable: true, active_sessions: None, busy_client_owned_session_count: None };
        assert!(confirm_daemon_session_loss(
            &probe,
            ConfirmOptions { force: false, copy: copy() },
            &io
        ));
        assert_eq!(
            prompt_seen.borrow().as_deref(),
            Some("The daemon is busy. Stop it anyway? [y/N] ")
        );

        let errors = RefCell::new(Vec::new());
        let error_fn = |message: &str| errors.borrow_mut().push(message.to_string());
        let io = ConfirmIo { stdin_is_tty: None, error: &error_fn, read_line: &|_: &str| "y".to_string() };
        assert!(!confirm_daemon_session_loss(
            &probe,
            ConfirmOptions { force: false, copy: copy() },
            &io
        ));
        assert_eq!(errors.borrow()[0], "\u{1b}[31mThe daemon is busy. Use --force.\u{1b}[39m");
    }

    #[test]
    fn pluralization_matches_the_typescript() {
        assert_eq!(pluralize_sessions(1).noun, "session");
        assert_eq!(pluralize_sessions(1).pronoun, "it");
        assert_eq!(pluralize_sessions(0).noun, "sessions");
        assert_eq!(pluralize_sessions(2).pronoun, "them");
    }
}
