use anyhow::{Context, Result};
use std::process::Command;

/// Severity levels to fetch, most severe first.
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

/// One severity group: its label, syslog priority and journald output lines.
pub struct Group {
    pub label: &'static str,
    pub priority: u8,
    /// Number of journald entries in this group.
    pub count: usize,
    pub lines: Vec<String>,
}

/// The current boot's critical/serious journald warnings, grouped by severity.
pub struct Report {
    pub groups: Vec<Group>,
    /// journalctl's stderr when it produced no entries.
    pub stderr: Option<String>,
}

/// Count journald entries per severity level for the current boot.
pub fn fetch_summary() -> Result<Summary> {
    let mut levels = Vec::new();
    for (label, priority) in LEVELS {
        let (stdout, _) = journalctl(priority)?;
        levels.push(Level {
            label,
            priority,
            count: count_entries(&stdout),
        });
    }
    Ok(Summary { levels })
}

/// Fetch journald messages with priority 0-3 (emerg, alert, crit, err) from the current boot.
pub fn fetch_boot_warnings() -> Result<Report> {
    let mut groups = Vec::new();
    let mut stderr = None;

    for (label, priority) in LEVELS {
        let (stdout, err) = journalctl(priority)?;
        if !err.trim().is_empty() && stderr.is_none() {
            stderr = Some(err.trim().to_string());
        }
        // journalctl prints "-- No entries --" on stdout when a level is empty.
        if is_no_entries(&stdout) {
            continue;
        }
        let entries = split_entries(&stdout);
        if entries.is_empty() {
            continue;
        }
        let count = entries.len();
        let lines = entries.into_iter().flatten().collect();
        groups.push(Group {
            label,
            priority,
            count,
            lines,
        });
    }

    Ok(Report { groups, stderr })
}

/// journalctl reports an empty result as this marker line on stdout.
fn is_no_entries(stdout: &str) -> bool {
    stdout.trim() == "-- No entries --"
}

/// Number of entries in a journalctl result, treating the empty marker as zero.
fn count_entries(stdout: &str) -> u32 {
    if is_no_entries(stdout) {
        0
    } else {
        split_entries(stdout).len() as u32
    }
}

/// Split journalctl `short` output into entries. journalctl indents continuation
/// lines of a multi-line message, so a line starting at column 0 begins a new entry.
fn split_entries(text: &str) -> Vec<Vec<String>> {
    let mut entries: Vec<Vec<String>> = Vec::new();
    for line in text.lines() {
        if !line.is_empty() && !line.starts_with(|c: char| c.is_whitespace()) {
            entries.push(vec![line.to_string()]);
        } else if let Some(last) = entries.last_mut() {
            last.push(line.to_string());
        }
    }
    entries
}

/// The `--priority` argument selecting exactly one severity level.
///
/// An exact range keeps each level disjoint. A bare "-p 3" would also match
/// every more severe level, duplicating entries across the severity groups.
fn priority_range(priority: u8) -> String {
    format!("{priority}..{priority}")
}

fn journalctl(priority: u8) -> Result<(String, String)> {
    let output = Command::new("journalctl")
        .arg("-p")
        .arg(priority_range(priority))
        .arg("-b")
        .arg("--no-pager")
        .arg("--no-hostname")
        .output()
        .context("Failed to run journalctl. Is systemd installed?")?;
    Ok((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_range_is_exact() {
        // A bare "-p 3" would also match priorities 0-2, duplicating entries
        // across the severity groups. Each level must be an exact range.
        for priority in 0..=3 {
            assert_eq!(priority_range(priority), format!("{priority}..{priority}"));
        }
    }

    #[test]
    fn test_split_entries_groups_continuation_lines() {
        let text = "Oct 19 21:34:14 kernel: first message\n\
                    Oct 19 21:34:15 kernel: second message\n\
                    \x20 continued line of second\n\
                    Oct 19 21:34:16 kernel: third message";

        let entries = split_entries(text);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].len(), 1);
        assert_eq!(entries[1].len(), 2);
        assert_eq!(entries[2].len(), 1);
    }

    #[test]
    fn test_split_entries_empty_input() {
        assert!(split_entries("").is_empty());
    }

    #[test]
    fn test_is_no_entries_detects_marker() {
        assert!(is_no_entries("-- No entries --"));
        assert!(is_no_entries("  -- No entries --  \n"));
        assert!(!is_no_entries("Oct 19 21:34:14 kernel: real message"));
        assert!(!is_no_entries(""));
    }

    #[test]
    fn test_count_entries_ignores_empty_marker() {
        // The marker is a real line, so it would otherwise count as one entry.
        assert_eq!(count_entries("-- No entries --"), 0);
        assert_eq!(count_entries(""), 0);
        assert_eq!(count_entries("Oct 19 21:34:14 kernel: real message"), 1);
    }

    #[test]
    fn test_fetch_boot_warnings_counts_match_combined_query() {
        // Requires a systemd journal on the host; skip where unavailable.
        let report = match fetch_boot_warnings() {
            Ok(report) => report,
            Err(_) => return,
        };

        for group in &report.groups {
            assert!(group.count > 0, "empty groups should be skipped");
            assert!(!group.lines.is_empty());
            assert!(group.priority <= 3, "only priorities 0-3 are fetched");
        }

        // The per-level groups must partition the combined result, so summing
        // them must equal a single -p 0..3 query. Any overlap would inflate
        // this total (the bug a bare "-p N" range would cause).
        let combined = Command::new("journalctl")
            .arg("-p")
            .arg("0..3")
            .arg("-b")
            .arg("--no-pager")
            .arg("--no-hostname")
            .output()
            .expect("failed to run journalctl");
        let combined = String::from_utf8_lossy(&combined.stdout).into_owned();

        if is_no_entries(&combined) {
            assert!(report.groups.is_empty());
            return;
        }

        let expected = split_entries(&combined).len();
        let actual: usize = report.groups.iter().map(|g| g.count).sum();
        assert_eq!(actual, expected, "severity groups must not overlap");
    }

    #[test]
    fn test_fetch_summary_counts_match_groups() {
        // Requires a systemd journal on the host; skip where unavailable.
        let summary = match fetch_summary() {
            Ok(summary) => summary,
            Err(_) => return,
        };
        let report = match fetch_boot_warnings() {
            Ok(report) => report,
            Err(_) => return,
        };

        assert_eq!(summary.levels.len(), LEVELS.len());
        for level in &summary.levels {
            let group = report.groups.iter().find(|g| g.priority == level.priority);
            match group {
                Some(group) => assert_eq!(level.count as usize, group.count),
                None => assert_eq!(level.count, 0),
            }
        }
    }
}
