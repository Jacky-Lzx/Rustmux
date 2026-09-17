//! Plain-text formatting for detailed session listings.

use std::time::{SystemTime, UNIX_EPOCH};

use unicode_width::UnicodeWidthStr;

use super::SessionInfo;

const HEADERS: [&str; 4] = ["SESSION", "STATUS", "PID", "LAST CONNECTED"];

pub(super) fn format_list() -> std::io::Result<String> {
    let mut sessions = super::list_info()?;
    super::order_info(&mut sessions, None);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    Ok(format_sessions(&sessions, now))
}

fn format_sessions(sessions: &[SessionInfo], now: u64) -> String {
    if sessions.is_empty() {
        return "no sessions\n".to_owned();
    }
    let rows: Vec<[String; 4]> = sessions
        .iter()
        .map(|session| {
            [
                session.name.to_string(),
                session.status(None).to_owned(),
                session
                    .server_pid
                    .map_or_else(|| "—".to_owned(), |pid| pid.to_string()),
                session.last_connected_label(None, now),
            ]
        })
        .collect();
    let mut widths = HEADERS.map(UnicodeWidthStr::width);
    for row in &rows {
        for (width, field) in widths.iter_mut().zip(row) {
            *width = (*width).max(UnicodeWidthStr::width(field.as_str()));
        }
    }

    let mut output = String::new();
    write_row(&mut output, &HEADERS, &widths);
    for row in &rows {
        let fields = row.each_ref().map(String::as_str);
        write_row(&mut output, &fields, &widths);
    }
    output
}

fn write_row(output: &mut String, fields: &[&str; 4], widths: &[usize; 4]) {
    for (index, field) in fields.iter().enumerate() {
        output.push_str(field);
        if index + 1 < fields.len() {
            let padding = widths[index] - UnicodeWidthStr::width(*field) + 2;
            output.push_str(&" ".repeat(padding));
        }
    }
    output.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionName;

    #[test]
    fn table_aligns_live_session_metadata_without_terminal_escapes() {
        let sessions = [
            info("active", true, Some(99_000), Some(1234)),
            info("long-detached", false, Some(40_000), Some(5678)),
            info("new", false, None, None),
        ];
        let output = format_sessions(&sessions, 100_000);
        assert_eq!(
            output,
            "SESSION        STATUS    PID   LAST CONNECTED\n\
             active         ATTACHED  1234  Now\n\
             long-detached  DETACHED  5678  1m ago\n\
             new            DETACHED  —     —\n"
        );
        assert!(!output.contains('\x1b'));
        assert_eq!(format_sessions(&[], 100_000), "no sessions\n");
    }

    fn info(
        name: &str,
        attached: bool,
        last_connected_at: Option<u64>,
        server_pid: Option<i32>,
    ) -> SessionInfo {
        SessionInfo {
            name: SessionName::new(name).unwrap(),
            attached,
            server_pid,
            last_connected_at,
        }
    }
}
