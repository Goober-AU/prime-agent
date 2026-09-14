//! Port of packages/coding-agent/src/cli/daemon-list-format.ts
//!
//! TODO(slice): `formatSessionDisplayId` (ca-session slice, core/session-id.ts)
//! and `SessionSummary` (ca-daemon-b slice, modes/daemon/daemon-session-list.ts)
//! are not landed. Private local stand-ins below keep the same behaviour and are
//! listed in the slice status file.

use serde::{Deserialize, Serialize};

// Display status derived from the lifecycle + activity axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListStatus {
    Working,
    Idle,
    Archived,
}

fn list_status_order(status: ListStatus) -> i32 {
    match status {
        ListStatus::Working => 0,
        ListStatus::Idle => 1,
        ListStatus::Archived => 2,
    }
}

fn list_status_for_summary(summary: &SessionSummary) -> ListStatus {
    if summary.lifecycle == "archived" {
        return ListStatus::Archived;
    }
    if summary.activity == "working" {
        ListStatus::Working
    } else {
        ListStatus::Idle
    }
}

struct ListRow {
    name: String,
    id: String,
    status: ListStatus,
    age: String,
    model: String,
    messages: String,
    clients: String,
}

pub fn format_session_list_table(sessions: &[SessionSummary], now_ms: f64) -> String {
    let rows: Vec<ListRow> = sort_sessions_for_list(sessions)
        .into_iter()
        .map(|session| ListRow {
            name: session.session_name.clone().unwrap_or_default(),
            id: format_session_display_id(&session.id),
            status: list_status_for_summary(session),
            age: format_session_age(session.modified.as_deref(), now_ms),
            model: format_session_model(session.model.as_ref()),
            messages: session.message_count.to_string(),
            clients: session.attached_clients.to_string(),
        })
        .collect();
    let columns = ["name", "id", "status", "age", "model", "messages", "clients"];
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            vec![
                row.name.clone(),
                row.id.clone(),
                row.status_text(),
                row.age.clone(),
                row.model.clone(),
                row.messages.clone(),
                row.clients.clone(),
            ]
        })
        .collect();
    // `formatListCell` runs on the padded cell, so the ANSI escapes never
    // influence the column widths.
    format_table(&columns, &cells, &|row_index, column_index, value| {
        if column_index != 2 {
            return value.to_string();
        }
        format_list_cell(&rows[row_index], value)
    })
}

fn sort_sessions_for_list(sessions: &[SessionSummary]) -> Vec<&SessionSummary> {
    let mut indexed: Vec<(usize, &SessionSummary)> = sessions.iter().enumerate().collect();
    indexed.sort_by_key(|(index, session)| (list_status_order(list_status_for_summary(session)), *index));
    indexed.into_iter().map(|(_, session)| session).collect()
}

fn format_list_cell(row: &ListRow, padded_value: &str) -> String {
    match row.status {
        ListStatus::Working => red(padded_value),
        ListStatus::Idle => blue(padded_value),
        ListStatus::Archived => dim(padded_value),
    }
}

impl ListRow {
    fn status_text(&self) -> String {
        match self.status {
            ListStatus::Working => "working".to_string(),
            ListStatus::Idle => "idle".to_string(),
            ListStatus::Archived => "archived".to_string(),
        }
    }
}

fn format_session_age(modified: Option<&str>, now_ms: f64) -> String {
    let modified = match modified {
        Some(modified) if !modified.is_empty() => modified,
        _ => return String::new(),
    };
    let modified_ms = match parse_iso8601_millis(modified) {
        Some(modified_ms) => modified_ms,
        None => return String::new(),
    };
    let age_seconds = ((now_ms - modified_ms) / 1000.0).floor().max(0.0);
    if age_seconds < 60.0 {
        return format!("{}s", age_seconds as i64);
    }
    let age_minutes = (age_seconds / 60.0).floor();
    if age_minutes < 60.0 {
        return format!("{}m", age_minutes as i64);
    }
    let age_hours = (age_minutes / 60.0).floor();
    if age_hours < 24.0 {
        return format!("{}h", age_hours as i64);
    }
    let age_days = (age_hours / 24.0).floor();
    if age_days < 7.0 {
        return format!("{}d", age_days as i64);
    }
    let age_weeks = (age_days / 7.0).floor();
    if age_weeks < 52.0 {
        return format!("{}w", age_weeks as i64);
    }
    format!("{}y", (age_weeks / 52.0).floor() as i64)
}

fn format_session_model(model: Option<&SessionModel>) -> String {
    match model {
        Some(model) => format!("{}/{}", model.provider, model.id),
        None => String::new(),
    }
}

/// Shared table renderer: column widths from the widest cell, two-space gutter.
fn format_table(
    columns: &[&str],
    rows: &[Vec<String>],
    format_cell: &dyn Fn(usize, usize, &str) -> String,
) -> String {
    let widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            rows.iter()
                .map(|row| row[index].chars().count())
                .chain(std::iter::once(column.chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut lines = vec![columns
        .iter()
        .enumerate()
        .map(|(index, column)| pad_end(column, widths[index]))
        .collect::<Vec<_>>()
        .join("  ")];
    for (row_index, row) in rows.iter().enumerate() {
        lines.push(
            row.iter()
                .enumerate()
                .map(|(index, value)| format_cell(row_index, index, &pad_end(value, widths[index])))
                .collect::<Vec<_>>()
                .join("  "),
        );
    }
    lines.join("\n")
}

fn pad_end(value: &str, width: usize) -> String {
    let length = value.chars().count();
    if length >= width {
        return value.to_string();
    }
    format!("{}{}", value, " ".repeat(width - length))
}

// ---------------------------------------------------------------------------
// Private local stand-ins for not-yet-landed slices.
// ---------------------------------------------------------------------------

/// Local stand-in for `SessionSummary` from
/// ../modes/daemon/daemon-session-list.js, narrowed to the fields this module
/// reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub lifecycle: String,
    pub activity: String,
    pub session_id: String,
    pub cwd: String,
    #[serde(rename = "sessionName", skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<SessionModel>,
    #[serde(rename = "attachedClients")]
    pub attached_clients: i64,
    #[serde(rename = "messageCount")]
    pub message_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionModel {
    pub provider: String,
    pub id: String,
}

/// Local stand-in for `formatSessionDisplayId` from
/// ../modes/daemon/daemon-session-id.js.
fn format_session_display_id(id: &str) -> String {
    const DISPLAY_ID_LENGTH: usize = 12;
    let normalized = normalize_hex_session_id(id);
    let effective = normalized.as_deref().unwrap_or(id);
    if effective.chars().count() > DISPLAY_ID_LENGTH {
        effective.chars().skip(effective.chars().count() - DISPLAY_ID_LENGTH).collect()
    } else {
        effective.to_string()
    }
}

fn normalize_hex_session_id(id: &str) -> Option<String> {
    let normalized: String = id.chars().filter(|ch| *ch != '-').map(|ch| ch.to_ascii_lowercase()).collect();
    if normalized.is_empty() || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(normalized)
}

/// `new Date(value).getTime()` for an ISO-8601 timestamp: milliseconds, or `None`
/// for `NaN`.
fn parse_iso8601_millis(value: &str) -> Option<f64> {
    let parsed = chrono::DateTime::parse_from_rfc3339(value).ok()?;
    Some(parsed.timestamp_millis() as f64)
}

fn red(value: &str) -> String {
    format!("\u{1b}[31m{}\u{1b}[39m", value)
}

fn blue(value: &str) -> String {
    format!("\u{1b}[34m{}\u{1b}[39m", value)
}

fn dim(value: &str) -> String {
    format!("\u{1b}[2m{}\u{1b}[22m", value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str, lifecycle: &str, activity: &str, modified: Option<&str>) -> SessionSummary {
        SessionSummary {
            id: id.to_string(),
            lifecycle: lifecycle.to_string(),
            activity: activity.to_string(),
            session_id: id.to_string(),
            cwd: "/tmp".to_string(),
            session_name: Some(format!("agent-{}", id)),
            model: Some(SessionModel { provider: "alpha".to_string(), id: "m1".to_string() }),
            attached_clients: 1,
            message_count: 2,
            modified: modified.map(str::to_string),
        }
    }

    #[test]
    fn renders_an_aligned_table_in_status_order() {
        let sessions = vec![
            summary("aaaaaaaaaaaa1", "live", "idle", Some("2026-01-01T00:00:00.000Z")),
            summary("aaaaaaaaaaaa2", "archived", "idle", None),
            summary("aaaaaaaaaaaa3", "live", "working", Some("2026-01-01T00:00:00.000Z")),
        ];
        let table = format_session_list_table(&sessions, 1_767_225_600_000.0);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines[0], "name                 id            status    age  model     messages  clients");
        assert!(lines[1].starts_with("agent-aaaaaaaaaaaa3"));
        assert!(lines[2].contains("idle"));
        assert!(lines[3].contains("archived"));
    }

    #[test]
    fn status_colour_wraps_the_padded_cell() {
        let sessions = vec![summary("aaaaaaaaaaaa1", "live", "working", None)];
        let table = format_session_list_table(&sessions, 0.0);
        let row = table.lines().nth(1).unwrap();
        assert!(row.contains("\u{1b}[31mworking\u{1b}[39m"), "{row}");
        let stripped = strip_ansi(row);
        assert_eq!(
            stripped,
            "agent-aaaaaaaaaaaa1  aaaaaaaaaaa1  working       alpha/m1  2         1      "
        );
    }

    fn strip_ansi(value: &str) -> String {
        let mut output = String::new();
        let mut chars = value.chars();
        while let Some(ch) = chars.next() {
            if ch == '\u{1b}' {
                for inner in chars.by_ref() {
                    if inner.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            output.push(ch);
        }
        output
    }

    #[test]
    fn formats_ages_at_every_unit_boundary() {
        let base = 1_767_225_600_000.0f64;
        let iso = |ms: f64| {
            chrono::DateTime::from_timestamp_millis(ms as i64)
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        };
        assert_eq!(format_session_age(Some(&iso(base - 5_000.0)), base), "5s");
        assert_eq!(format_session_age(Some(&iso(base - 60_000.0)), base), "1m");
        assert_eq!(format_session_age(Some(&iso(base - 3_600_000.0)), base), "1h");
        assert_eq!(format_session_age(Some(&iso(base - 86_400_000.0)), base), "1d");
        assert_eq!(format_session_age(Some(&iso(base - 604_800_000.0)), base), "1w");
        assert_eq!(format_session_age(Some(&iso(base - 31_536_000_000.0)), base), "1y");
        assert_eq!(format_session_age(None, base), "");
        assert_eq!(format_session_age(Some("not-a-date"), base), "");
        assert_eq!(format_session_age(Some(&iso(base + 5_000.0)), base), "0s");
    }

    #[test]
    fn formats_the_model_pair() {
        assert_eq!(
            format_session_model(Some(&SessionModel { provider: "p".to_string(), id: "m".to_string() })),
            "p/m"
        );
        assert_eq!(format_session_model(None), "");
    }

    #[test]
    fn display_ids_truncate_to_twelve_characters() {
        assert_eq!(format_session_display_id("0123456789abcdef"), "456789abcdef");
        assert_eq!(format_session_display_id("abc"), "abc");
        assert_eq!(format_session_display_id("0123-4567-89ab-cdef"), "456789abcdef");
        assert_eq!(format_session_display_id("zzzzzzzzzzzzzzzzzz"), "zzzzzzzzzzzz");
    }
}
