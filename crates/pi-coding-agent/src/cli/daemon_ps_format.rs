//! Port of packages/coding-agent/src/cli/daemon-ps-format.ts

use super::daemon_ps::{DaemonInfo, DaemonStatus};

struct DaemonRow {
    socket: String,
    pid: String,
    version: String,
    status: String,
    sessions: String,
    uptime: String,
}

pub fn format_daemon_list_table(daemons: &[DaemonInfo]) -> String {
    let rows: Vec<DaemonRow> = daemons
        .iter()
        .map(|daemon| DaemonRow {
            socket: if daemon.is_default {
                format!("{} *", daemon.socket_path)
            } else {
                daemon.socket_path.clone()
            },
            pid: match daemon.pid {
                Some(pid) => pid.to_string(),
                None => String::new(),
            },
            version: daemon.version.clone().unwrap_or_default(),
            status: daemon.status.as_str().to_string(),
            sessions: match daemon.session_count {
                Some(session_count) => session_count.to_string(),
                None => String::new(),
            },
            uptime: format_uptime(daemon.uptime_seconds),
        })
        .collect();
    let columns = ["socket", "pid", "version", "status", "sessions", "uptime"];
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            vec![
                row.socket.clone(),
                row.pid.clone(),
                row.version.clone(),
                row.status.clone(),
                row.sessions.clone(),
                row.uptime.clone(),
            ]
        })
        .collect();
    // `formatDaemonCell` only colors the "status" column, and it colors the
    // padded cell, so the padding happens before the color codes are applied.
    let table = format_table_with_color(&columns, &cells, Some(3));
    if daemons.iter().any(|daemon| daemon.is_default) {
        format!("{}\n\n{}", table, dim("* default background service"))
    } else {
        table
    }
}

fn format_daemon_cell(status: DaemonStatus, value: &str) -> String {
    color_status(status, value)
}

fn daemon_status_from_row(value: &str) -> DaemonStatus {
    DaemonStatus::from_str(value).unwrap_or(DaemonStatus::OrphanFile)
}

fn color_status(status: DaemonStatus, value: &str) -> String {
    match status {
        DaemonStatus::Current => green(value),
        DaemonStatus::Stale => yellow(value),
        DaemonStatus::Unreachable => red(value),
        DaemonStatus::OrphanFile => dim(value),
    }
}

pub fn format_uptime(uptime_seconds: Option<f64>) -> String {
    let uptime_seconds = match uptime_seconds {
        Some(uptime_seconds) if uptime_seconds.is_finite() => uptime_seconds,
        _ => return String::new(),
    };
    let seconds = uptime_seconds.floor().max(0.0);
    if seconds < 60.0 {
        return format!("{}s", seconds as i64);
    }
    let minutes = (seconds / 60.0).floor();
    if minutes < 60.0 {
        return format!("{}m", minutes as i64);
    }
    let hours = (minutes / 60.0).floor();
    if hours < 24.0 {
        return format!("{}h", hours as i64);
    }
    let days = (hours / 24.0).floor();
    if days < 7.0 {
        return format!("{}d", days as i64);
    }
    format!("{}w", (days / 7.0).floor() as i64)
}

/// Shared table renderer: column widths from the widest cell, two-space gutter.
fn format_table(columns: &[&str], rows: &[Vec<String>]) -> String {
    format_table_with_color(columns, rows, None)
}

fn format_table_with_color(columns: &[&str], rows: &[Vec<String>], color_column: Option<usize>) -> String {
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
    for row in rows {
        lines.push(
            row.iter()
                .enumerate()
                .map(|(index, value)| {
                    let padded = pad_end(value, widths[index]);
                    match color_column {
                        Some(column) if column == index => {
                            format_daemon_cell(daemon_status_from_row(value), &padded)
                        }
                        _ => padded,
                    }
                })
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

fn red(value: &str) -> String {
    format!("\u{1b}[31m{}\u{1b}[39m", value)
}

fn green(value: &str) -> String {
    format!("\u{1b}[32m{}\u{1b}[39m", value)
}

fn yellow(value: &str) -> String {
    format!("\u{1b}[33m{}\u{1b}[39m", value)
}

fn dim(value: &str) -> String {
    format!("\u{1b}[2m{}\u{1b}[22m", value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon(socket_path: &str, status: DaemonStatus, is_default: bool) -> DaemonInfo {
        DaemonInfo {
            socket_path: socket_path.to_string(),
            pid: Some(42),
            uptime_seconds: Some(90.0),
            version: Some("0.9.3".to_string()),
            protocol_version: Some(7.0),
            schema_id: Some("protocol-7-schema-29".to_string()),
            build_id: Some("build".to_string()),
            executable_path: None,
            pid_source: Some("listener".to_string()),
            session_count: Some(2),
            status,
            is_default,
            has_tracked_workers: None,
        }
    }

    #[test]
    fn formats_uptime_at_every_unit_boundary() {
        assert_eq!(format_uptime(None), "");
        assert_eq!(format_uptime(Some(f64::NAN)), "");
        assert_eq!(format_uptime(Some(-5.0)), "0s");
        assert_eq!(format_uptime(Some(59.9)), "59s");
        assert_eq!(format_uptime(Some(60.0)), "1m");
        assert_eq!(format_uptime(Some(3600.0)), "1h");
        assert_eq!(format_uptime(Some(86_400.0)), "1d");
        assert_eq!(format_uptime(Some(604_800.0)), "1w");
    }

    #[test]
    fn marks_the_default_socket_and_appends_the_legend() {
        let table = format_daemon_list_table(&[daemon("/tmp/daemon.sock", DaemonStatus::Current, true)]);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines[0], "socket             pid  version  status   sessions  uptime");
        assert!(lines[1].starts_with("/tmp/daemon.sock *"));
        assert!(lines[1].contains("\u{1b}[32mcurrent  \u{1b}[39m"));
        assert_eq!(lines[3], "\u{1b}[2m* default background service\u{1b}[22m");
    }

    #[test]
    fn omits_missing_values_and_the_legend() {
        let mut info = daemon("/tmp/other.sock", DaemonStatus::OrphanFile, false);
        info.pid = None;
        info.version = None;
        info.session_count = None;
        info.uptime_seconds = None;
        let table = format_daemon_list_table(&[info]);
        assert!(!table.contains("* default background service"));
        let row = table.lines().nth(1).unwrap();
        assert!(row.starts_with("/tmp/other.sock   "));
        assert!(row.contains("\u{1b}[2morphan-file\u{1b}[22m"));
    }

    #[test]
    fn colors_every_status() {
        assert!(format_daemon_cell(DaemonStatus::Current, "current").starts_with("\u{1b}[32m"));
        assert!(format_daemon_cell(DaemonStatus::Stale, "stale").starts_with("\u{1b}[33m"));
        assert!(format_daemon_cell(DaemonStatus::Unreachable, "unreachable").starts_with("\u{1b}[31m"));
        assert!(format_daemon_cell(DaemonStatus::OrphanFile, "orphan-file").starts_with("\u{1b}[2m"));
    }
}
