use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::process::Command;

/// Severity levels shown in the header summary, most severe first.
const LEVELS: [(&str, u8); 4] = [
    ("EMERGENCY", 0),
    ("ALERT", 1),
    ("CRITICAL", 2),
    ("ERROR", 3),
];

/// Journald entry count for one severity level.
pub struct Level {
    pub label: &'static str,
    pub priority: u8,
    pub count: u32,
}

/// Journald entry counts for the current boot, most severe first.
pub struct Summary {
    pub levels: Vec<Level>,
}

/// One severity group: many journald entries collapsed into a single row.
pub struct Group {
    pub priority: u8,
    pub source: String,
    /// First human-readable line, used for the collapsed row.
    pub message: String,
    /// All non-backtrace lines of the message, for the expanded row.
    pub detail: String,
    pub count: usize,
    /// Timestamps (local, `MM-DD HH:MM`) of the first and last occurrence.
    pub first: String,
    pub last: String,
    /// Backtrace frame lines collapsed into this group.
    pub frames: usize,
}

/// The current boot's critical/serious journald warnings, grouped by
/// severity, source and message.
pub struct Report {
    pub groups: Vec<Group>,
    pub summary: Summary,
    /// Journald entries read, before grouping.
    pub total: usize,
}

/// Count journald entries per severity level for the current boot.
pub fn fetch_summary() -> Result<Summary> {
    Ok(fetch_report()?.summary)
}

/// Fetch priority 0-3 messages for the current boot and group them.
pub fn fetch_report() -> Result<Report> {
    let output = Command::new("journalctl")
        .args([
            "-p",
            "0..3",
            "-b",
            "--no-pager",
            "-o",
            "json",
            "--output-fields=MESSAGE,PRIORITY,SYSLOG_IDENTIFIER,_SYSTEMD_UNIT,__REALTIME_TIMESTAMP",
        ])
        .output()
        .context("Failed to run journalctl. Is systemd installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("journalctl failed: {}", stderr.trim());
    }

    Ok(parse_report(&String::from_utf8_lossy(&output.stdout)))
}

/// Parse `journalctl -o json` output into grouped rows.
fn parse_report(stdout: &str) -> Report {
    let mut counts = [0u32; LEVELS.len()];
    let mut entries: Vec<Parsed> = Vec::new();
    let mut total = 0;

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => continue,
        };

        // Entries without a message (coredump metadata rows) carry no text.
        let message = match value.get("MESSAGE").and_then(Value::as_str) {
            Some(message) if !message.trim().is_empty() => message,
            _ => continue,
        };

        let priority = value
            .get("PRIORITY")
            .and_then(|v| {
                v.as_str()
                    .and_then(|s| s.parse::<u8>().ok())
                    .or_else(|| v.as_u64().map(|n| n as u8))
            })
            .unwrap_or(3);
        total += 1;
        if let Some(slot) = counts.get_mut(priority as usize) {
            *slot += 1;
        }

        let source = value
            .get("SYSLOG_IDENTIFIER")
            .and_then(Value::as_str)
            .or_else(|| value.get("_SYSTEMD_UNIT").and_then(Value::as_str))
            .map(|s| s.trim_end_matches(".service").to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let time = value
            .get("__REALTIME_TIMESTAMP")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<u64>().ok())
            .map(format_time)
            .unwrap_or_else(|| "--:--".to_string());

        let (header, detail, frames) = split_frames(message);
        match header {
            Some(header) => entries.push(Parsed {
                priority,
                source,
                message: header,
                detail,
                time,
                frames,
            }),
            // A pure backtrace belongs to the entry that produced it.
            None => {
                if let Some(previous) = entries.last_mut() {
                    previous.frames += frames;
                }
            }
        }
    }

    let mut groups: Vec<Group> = Vec::new();
    let mut index: HashMap<(u8, String, String), usize> = HashMap::new();

    for entry in entries {
        let key = (
            entry.priority,
            entry.source.clone(),
            normalize(&entry.message),
        );
        match index.get(&key) {
            Some(&i) => {
                let group = &mut groups[i];
                group.count += 1;
                group.last = entry.time;
                group.frames += entry.frames;
            }
            None => {
                index.insert(key, groups.len());
                groups.push(Group {
                    priority: entry.priority,
                    source: entry.source,
                    message: entry.message,
                    detail: entry.detail,
                    count: 1,
                    first: entry.time.clone(),
                    last: entry.time,
                    frames: entry.frames,
                });
            }
        }
    }

    groups.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then(b.count.cmp(&a.count))
            .then(a.source.cmp(&b.source))
    });

    let summary = Summary {
        levels: LEVELS
            .iter()
            .map(|(label, priority)| Level {
                label,
                priority: *priority,
                count: counts[*priority as usize],
            })
            .collect(),
    };

    Report {
        groups,
        summary,
        total,
    }
}

struct Parsed {
    priority: u8,
    source: String,
    message: String,
    detail: String,
    time: String,
    frames: usize,
}

/// Split a message into its first human-readable line and the remaining text,
/// counting backtrace frame lines (`#0 0x…`, `(libfoo.so + 0x…)`).
fn split_frames(message: &str) -> (Option<String>, String, usize) {
    let mut first = None;
    let mut detail = Vec::new();
    let mut frames = 0;

    for line in message.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if is_frame(line) {
            frames += 1;
            continue;
        }
        detail.push(line.to_string());
        if first.is_none() {
            first = Some(line.to_string());
        }
    }

    (first, detail.join("\n"), frames)
}

/// Backtrace frames as printed by systemd for crashes.
fn is_frame(line: &str) -> bool {
    let line = line.trim_start();
    (line.starts_with('#') && line.contains(" 0x")) || (line.starts_with('(') && line.contains(" + 0x"))
}

/// Grouping key for a message: hex addresses and pointers are the only thing
/// that changes between two occurrences of the same error.
fn normalize(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;

    while let Some(position) = rest.find("0x") {
        out.push_str(&rest[..position]);
        let after = &rest[position + 2..];
        if after.starts_with(|c: char| c.is_ascii_hexdigit()) {
            out.push_str("0x…");
            rest = after.trim_start_matches(|c: char| c.is_ascii_hexdigit());
        } else {
            out.push_str("0x");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Local `MM-DD HH:MM` for a journald `__REALTIME_TIMESTAMP` (microseconds).
fn format_time(usec: u64) -> String {
    let seconds: libc::time_t = (usec / 1_000_000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::localtime_r(&seconds, &mut tm) };
    if result.is_null() {
        return "--:--".to_string();
    }
    format!(
        "{:02}-{:02} {:02}:{:02}",
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
{"MESSAGE":"src/profile.c:record_cb() No SDP records found for Hands-Free Voice gateway","PRIORITY":"3","SYSLOG_IDENTIFIER":"bluetoothd","__REALTIME_TIMESTAMP":"1792924524000000"}
{"MESSAGE":"src/profile.c:record_cb() No SDP records found for Hands-Free Voice gateway","PRIORITY":"3","SYSLOG_IDENTIFIER":"bluetoothd","__REALTIME_TIMESTAMP":"1792924624000000"}
{"MESSAGE":"iwlwifi 0000:00:14.3: BIOS contains WGDS but no WRDS","PRIORITY":"3","SYSLOG_IDENTIFIER":"kernel","__REALTIME_TIMESTAMP":"1792924724000000"}
{"PRIORITY":"2","COREDUMP_PID":"4712","__REALTIME_TIMESTAMP":"1792924524157597"}
{"MESSAGE":"Process 4712 (wireplumber) of user 1000 dumped core.\n#0  0x00007f2437354d58 n/a (libspa-bluez5.so + 0x4fd58)\n#1  0x00007f245a6c58ce n/a (libpipewire-0.3.so.0 + 0x6c8ce)","PRIORITY":"2","SYSLOG_IDENTIFIER":"systemd-coredump","__REALTIME_TIMESTAMP":"1792924524000000"}
"#;

    #[test]
    fn test_parse_report_groups_repeated_messages() {
        let report = parse_report(FIXTURE);
        assert_eq!(report.total, 4, "null-MESSAGE entries must be skipped");
        assert_eq!(report.groups.len(), 3);

        let bluetooth = report
            .groups
            .iter()
            .find(|g| g.source == "bluetoothd")
            .expect("bluetoothd group");
        assert_eq!(bluetooth.count, 2);
        assert_eq!(bluetooth.priority, 3);
        assert!(bluetooth.first < bluetooth.last);
    }

    #[test]
    fn test_backtrace_frames_are_collapsed() {
        let report = parse_report(FIXTURE);
        let crash = report
            .groups
            .iter()
            .find(|g| g.source == "systemd-coredump")
            .expect("coredump group");
        assert_eq!(crash.frames, 2);
        assert!(!crash.message.contains("0x"));
        assert!(!crash.detail.contains("0x"));
        assert!(crash.message.contains("dumped core"));
    }

    #[test]
    fn test_summary_counts_match_entries() {
        let report = parse_report(FIXTURE);
        let total: u32 = report.summary.levels.iter().map(|l| l.count).sum();
        assert_eq!(total as usize, report.total);
        assert_eq!(
            report
                .summary
                .levels
                .iter()
                .find(|l| l.priority == 2)
                .unwrap()
                .count,
            1
        );
    }

    #[test]
    fn test_normalize_collapses_addresses() {
        assert_eq!(
            normalize("segfault at 0x4421 ip 0x7f00 sp 0xdead error 4"),
            "segfault at 0x… ip 0x… sp 0x… error 4"
        );
        assert_eq!(normalize("no addresses here"), "no addresses here");
    }

    #[test]
    fn test_split_frames_ignores_pure_backtrace() {
        let (first, detail, frames) = split_frames("#0 0x1234 n/a (libfoo.so + 0x1)\n#1 0x5678");
        assert!(first.is_none());
        assert!(detail.is_empty());
        assert_eq!(frames, 2);
    }

    #[test]
    fn test_fetch_report_summary_matches_total() {
        // Requires a systemd journal on the host; skip where unavailable.
        let report = match fetch_report() {
            Ok(report) => report,
            Err(_) => return,
        };

        for group in &report.groups {
            assert!(!group.detail.is_empty());
            assert!(group.priority <= 3, "only priorities 0-3 are fetched");
            assert!(group.count > 0);
        }

        let total: u32 = report.summary.levels.iter().map(|l| l.count).sum();
        assert_eq!(total as usize, report.total);
    }
}
