use anyhow::{Context, Result};
use std::fs;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::steps::Step;

/// Marker that starts each step's block in update.sh. Steps run from their
/// marker up to the next step's marker (`# Post-Upgrade Maintenance` for the
/// upgrade). Both are comments or section headers, so a step is always a
/// complete, runnable slice of the script.
const STEP_MARKERS: [(&str, &str); 9] = [
    ("Pre-flight checks", "# Pre-flight Checks"),
    ("System upgrade", "section \"Package Upgrade\""),
    ("Pacnew/pacsave check", "# Post-Upgrade Maintenance"),
    ("Orphaned packages", "# 2. Check for orphaned packages"),
    ("Pacman cache cleanup", "# 3. Pacman cache maintenance"),
    ("Trash cleanup", "# 4. Empty user trash"),
    ("Broken symlinks", "# 5. Check for broken symbolic links"),
    ("Failed systemd services", "# 6. Failed systemd services check"),
    ("Reboot detection", "section \"Reboot Status\""),
];

/// Locate a step's line range (1-indexed, inclusive) in update.sh.
///
/// A missing marker is an error: silently emitting an empty step would make
/// the TUI report success for maintenance that never ran.
fn step_lines(lines: &[&str], name: &str) -> Result<(usize, usize)> {
    let index = STEP_MARKERS
        .iter()
        .position(|(step, _)| *step == name)
        .with_context(|| format!("unknown step '{}'", name))?;
    let marker = STEP_MARKERS[index].1;

    let start = lines
        .iter()
        .position(|line| line.contains(marker))
        .with_context(|| format!("marker '{}' for step '{}' not found", marker, name))?;

    let end_marker = if name == "System upgrade" {
        Some("# Post-Upgrade Maintenance")
    } else {
        STEP_MARKERS.get(index + 1).map(|(_, marker)| *marker)
    };
    let end = end_marker
        .and_then(|marker| lines.iter().position(|line| line.contains(marker)))
        .unwrap_or(lines.len());

    if start >= end {
        anyhow::bail!(
            "step '{}' has an empty range in update.sh (marker at line {}, next marker at line {})",
            name,
            start + 1,
            end + 1
        );
    }

    Ok((start + 1, end))
}

/// A generated script plus the file its per-step trailers are written to.
pub struct Prepared {
    script: tempfile::NamedTempFile,
    status: tempfile::NamedTempFile,
}

pub struct RunOutcome {
    pub status: ExitStatus,
    pub ran: Vec<String>,
    pub duration: Duration,
}

impl Prepared {
    #[cfg(test)]
    pub fn script_path(&self) -> &std::path::Path {
        self.script.path()
    }

    pub fn script_text(&self) -> Result<String> {
        fs::read_to_string(self.script.path())
            .with_context(|| format!("Failed to read {}", self.script.path().display()))
    }

    /// Run the script interactively, inheriting the terminal so sudo, pacman
    /// and AUR helper prompts work normally.
    pub fn run(&self, auto_confirm: Option<bool>) -> Result<RunOutcome> {
        let mut cmd = Command::new("bash");
        cmd.arg(self.script.path());

        // If auto_confirm is Some(true), pass --noconfirm for pacman/yay
        if auto_confirm == Some(true) {
            cmd.arg("--noconfirm");
        }

        cmd.env("ARCHCTL_STATUS_FILE", self.status.path());
        cmd.stdin(Stdio::inherit());
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());

        let started = Instant::now();
        let status = cmd
            .status()
            .with_context(|| format!("Failed to execute script at {}", self.script.path().display()))?;
        let duration = started.elapsed();

        let ran = fs::read_to_string(self.status.path())
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.strip_prefix("ran "))
            .map(str::to_string)
            .collect();

        Ok(RunOutcome {
            status,
            ran,
            duration,
        })
    }
}

/// Generate a bash script that sources helper functions from update.sh,
/// ensures all safe variable defaults are defined, configures auto-confirmation
/// if requested, and runs the selected steps.
pub fn generate_script(
    update_script: &str,
    selected: &[&Step],
    auto_confirm: Option<bool>,
) -> Result<Prepared> {
    let content = fs::read_to_string(update_script)
        .with_context(|| format!("Failed to read {}", update_script))?;
    let lines: Vec<&str> = content.lines().collect();

    // Everything up to the pre-flight marker is the shared prologue: shebang,
    // colours and helper functions.
    let header_end = lines
        .iter()
        .position(|line| line.contains(STEP_MARKERS[0].1))
        .context("Could not find the pre-flight section in update.sh")?;

    let mut script = String::new();
    for line in &lines[..header_end] {
        script.push_str(line);
        script.push('\n');
    }
    script.push('\n');

    // Safe variable initialization defaults (avoids bash set -u unbound
    // variable crashes when the pre-flight step is not selected).
    script.push_str(
        r#"
# --- archctl environment initialization ---
ARCHCTL_STATUS_FILE="${ARCHCTL_STATUS_FILE:-/dev/null}"
HELPER=""
if command -v yay >/dev/null 2>&1; then
    HELPER="yay"
elif command -v paru >/dev/null 2>&1; then
    HELPER="paru"
elif command -v pacman >/dev/null 2>&1; then
    HELPER="sudo pacman"
fi
PACMAN_LOG="/var/log/pacman.log"
LOG_START_LINE=0
if [[ -f "$PACMAN_LOG" ]]; then
    LOG_START_LINE=$(wc -l < "$PACMAN_LOG" 2>/dev/null || echo 0)
fi
PACMAN_DB_LCK="/var/lib/pacman/db.lck"
"#,
    );

    if let Some(confirm) = auto_confirm {
        script.push_str(&auto_confirm_override(confirm));
    }

    for step in selected {
        let (start, end) = step_lines(&lines, step.name)?;
        script.push_str(&format!("\n# === {} ===\n", step.name));
        for line in &lines[start - 1..end] {
            script.push_str(line);
            script.push('\n');
        }
        // Trailers let the TUI report which steps actually ran. A step that
        // exits early never reaches its trailer.
        script.push_str(&format!(
            "printf 'ran %s\\n' '{}' >> \"$ARCHCTL_STATUS_FILE\"\n",
            step.name
        ));
    }

    let mut script_file = tempfile::NamedTempFile::new()
        .context("Failed to create a temporary script file")?;
    std::io::Write::write_all(&mut script_file, script.as_bytes())
        .context("Failed to write the temporary script")?;

    let status_file = tempfile::NamedTempFile::new()
        .context("Failed to create a temporary status file")?;

    Ok(Prepared {
        script: script_file,
        status: status_file,
    })
}

/// Overrides bash's `read` builtin so every prompt in the script is answered
/// automatically. Reads whose stdin is redirected (the `while read` loops that
/// parse command output) are passed through to the real builtin, otherwise
/// they would never see EOF and would loop forever.
fn auto_confirm_override(confirm: bool) -> String {
    let answer = if confirm { "y" } else { "n" };
    let message = if confirm {
        "auto-confirmed (yes)"
    } else {
        "auto-declined (no)"
    };
    let return_code = if confirm { 0 } else { 1 };

    format!(
        r#"
# --- archctl auto-confirm override ---
read() {{
    if [[ ! -t 0 ]]; then
        builtin read "$@"
        return $?
    fi
    local prompt="" var="" opt
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --) shift; break ;;
            -a|-d|-i|-n|-N|-p|-t|-u)
                opt="$1"; shift
                if [[ $# -gt 0 ]]; then
                    [[ "$opt" == "-p" ]] && prompt="$1"
                    shift
                fi
                ;;
            -*) shift ;;
            *) var="$1"; shift ;;
        esac
    done
    if [[ -n "$prompt" ]]; then
        printf "%s" "$prompt"
    fi
    echo "{ans} [{msg}]"
    if [[ -n "$var" ]]; then
        printf -v "$var" "%s" "{ans}"
    else
        REPLY="{ans}"
    fi
    return {code}
}}
"#,
        ans = answer,
        msg = message,
        code = return_code,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn update_script() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("update.sh");
        assert!(path.exists(), "update.sh must exist for the script tests");
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn test_all_steps_produce_a_body() {
        let path = update_script();
        for step in Step::all() {
            let prepared = generate_script(&path, &[&step], None).unwrap();
            let script = prepared.script_text().unwrap();
            let header = format!("# === {} ===", step.name);
            let body = script
                .split(&header)
                .nth(1)
                .unwrap_or_else(|| panic!("step '{}' missing from generated script", step.name));
            let real_lines = body
                .lines()
                .filter(|line| {
                    let line = line.trim();
                    !line.is_empty() && !line.starts_with("printf 'ran ")
                })
                .count();
            assert!(
                real_lines > 0,
                "step '{}' generated an empty body",
                step.name
            );
        }
    }

    #[test]
    fn test_missing_marker_is_an_error() {
        let path = update_script();
        let prepared = generate_script(&path, &[&Step::all()[0]], None).unwrap();
        let mut broken = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut broken, b"#!/usr/bin/env bash\necho hi\n").unwrap();
        let result = generate_script(broken.path().to_str().unwrap(), &[&Step::all()[0]], None);
        assert!(result.is_err(), "a script without markers must be rejected");
        drop(prepared);
    }

    #[test]
    fn test_auto_confirm_override_leaves_piped_reads_alone() {
        // `while read` loops that parse command output must still see EOF.
        let mut script = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(
            &mut script,
            format!(
                "{}\nwhile IFS= read -r line; do printf 'got:%s\\n' \"$line\"; done < <(printf 'a\\nb\\n')\n",
                auto_confirm_override(true)
            )
            .as_bytes(),
        )
        .unwrap();

        let output = Command::new("bash")
            .arg(script.path())
            .output()
            .expect("failed to run bash");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("got:a"), "stdout: {}", stdout);
        assert!(stdout.contains("got:b"), "stdout: {}", stdout);
    }

    #[test]
    fn test_generated_script_is_valid_bash() {
        let path = update_script();
        let steps = Step::all();
        let selected: Vec<&Step> = steps.iter().collect();

        for confirm in [None, Some(true), Some(false)] {
            let prepared = generate_script(&path, &selected, confirm).unwrap();
            let status = Command::new("bash")
                .arg("-n")
                .arg(prepared.script_path())
                .status()
                .expect("Failed to run bash -n");
            assert!(status.success(), "Generated script failed bash syntax check");
        }
    }

    #[test]
    fn test_trailers_are_in_selection_order() {
        let path = update_script();
        let steps = Step::all();
        let selected = vec![&steps[3], &steps[4]];
        let prepared = generate_script(&path, &selected, None).unwrap();
        let script = prepared.script_text().unwrap();

        let orphans = script.rfind("'Orphaned packages'").unwrap();
        let cache = script.rfind("'Pacman cache cleanup'").unwrap();
        assert!(orphans < cache, "trailers must follow the selection order");
    }
}
