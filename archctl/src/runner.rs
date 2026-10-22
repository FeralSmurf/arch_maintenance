use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::steps::Step;

/// Fallback line ranges in update.sh (1-indexed, inclusive) for each step.
fn fallback_step_lines(name: &str) -> Option<(usize, usize)> {
    match name {
        "Pre-flight checks" => Some((170, 210)),
        "System upgrade" => Some((215, 224)),
        "Pacnew/pacsave check" => Some((229, 252)),
        "Orphaned packages" => Some((254, 273)),
        "Pacman cache cleanup" => Some((275, 291)),
        "Trash cleanup" => Some((293, 294)),
        "Broken symlinks" => Some((296, 297)),
        "Failed systemd services" => Some((299, 309)),
        "Reboot detection" => Some((311, 361)),
        _ => None,
    }
}

/// Find step line ranges dynamically based on markers in the script,
/// falling back to predefined line ranges if markers cannot be found.
fn get_step_lines(lines: &[&str], name: &str) -> Option<(usize, usize)> {
    let marker_list = [
        ("Pre-flight checks", "# 1. Determine package manager"),
        ("System upgrade", "section \"Package Upgrade\""),
        ("Pacnew/pacsave check", "# 1. Check for .pacnew and .pacsave files"),
        ("Orphaned packages", "# 2. Check for orphaned packages"),
        ("Pacman cache cleanup", "# 3. Pacman cache maintenance"),
        ("Trash cleanup", "# 4. Empty user trash"),
        ("Broken symlinks", "# 5. Check for broken symbolic links"),
        ("Failed systemd services", "# 6. Failed systemd services check"),
        ("Reboot detection", "section \"Reboot Status\""),
    ];

    let target_idx = marker_list.iter().position(|(n, _)| *n == name)?;
    let target_marker = marker_list[target_idx].1;

    let start_line = lines.iter().position(|line| line.contains(target_marker))?;

    // The upgrade step ends where post-upgrade maintenance begins, before the
    // next selectable step's marker.
    let end_marker = if name == "System upgrade" {
        Some("# Post-Upgrade Maintenance")
    } else {
        marker_list.get(target_idx + 1).map(|(_, marker)| *marker)
    };
    let end_line = end_marker
        .and_then(|marker| lines.iter().position(|line| line.contains(marker)))
        .unwrap_or(lines.len());

    if start_line < end_line {
        let mut actual_start = start_line;
        if (name == "Pre-flight checks" && actual_start >= 2 && lines[actual_start - 2].contains("banner"))
            || (name == "Pacnew/pacsave check"
                && actual_start >= 2
                && lines[actual_start - 2].contains("Maintenance & Diagnostics"))
        {
            actual_start -= 2;
        }
        Some((actual_start + 1, end_line))
    } else {
        fallback_step_lines(name)
    }
}

/// Generate a bash script that sources helper functions from update.sh,
/// ensures all safe variable defaults are defined, configures auto-confirmation
/// if requested, and runs the selected steps.
pub fn generate_script(
    update_script: &str,
    selected: &[&Step],
    auto_confirm: Option<bool>,
) -> Result<PathBuf> {
    let content = fs::read_to_string(update_script)
        .with_context(|| format!("Failed to read {}", update_script))?;
    let lines: Vec<&str> = content.lines().collect();

    let mut script = String::new();

    // 1. Header: shebang, colors, and utility function definitions from update.sh
    let header_end = lines
        .iter()
        .position(|l| l.contains("# Pre-flight Checks") || l.contains("# 1. Determine package manager"))
        .unwrap_or_else(|| 166.min(lines.len()));

    for line in &lines[..header_end] {
        script.push_str(line);
        script.push('\n');
    }
    script.push('\n');

    // 2. Safe variable initialization defaults (avoids bash set -u unbound variable crashes)
    script.push_str(r#"
# --- archctl environment initialization ---
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
"#);

    // 3. Auto-confirmation overrides if configured
    if let Some(confirm) = auto_confirm {
        let answer = if confirm { "y" } else { "n" };
        let msg = if confirm { "auto-confirmed (yes)" } else { "auto-declined (no)" };
        script.push_str(&format!(
            r#"
# --- archctl auto-confirm override ---
read() {{
    local prompt=""
    local var=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            -p) prompt="$2"; shift 2 ;;
            -r|-s|-n|-d|-t) shift ;;
            *) var="$1"; shift ;;
        esac
    done
    if [[ -n "$prompt" ]]; then
        printf "%s" "$prompt"
    fi
    echo "{ans} [{msg}]"
    if [[ -n "$var" ]]; then
        printf -v "$var" "{ans}"
    else
        REPLY="{ans}"
    fi
    return 0
}}
"#,
            ans = answer,
            msg = msg,
        ));
    }

    // 4. Selected steps
    for step in selected {
        script.push_str(&format!("\n# === {} ===\n", step.name));
        if let Some((start, end)) = get_step_lines(&lines, step.name) {
            let start_idx = start.saturating_sub(1);
            let end_idx = end.min(lines.len());
            if start_idx < end_idx {
                for line in &lines[start_idx..end_idx] {
                    script.push_str(line);
                    script.push('\n');
                }
            }
        }
    }

    // 5. Write to temp file
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp_dir = std::env::temp_dir();
    let script_path = temp_dir.join(format!("archctl-{}-{}.sh", std::process::id(), timestamp));

    fs::write(&script_path, &script)
        .with_context(|| format!("Failed to write script to {}", script_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = fs::metadata(&script_path) {
            let mut perms = metadata.permissions();
            perms.set_mode(0o755);
            let _ = fs::set_permissions(&script_path, perms);
        }
    }

    Ok(script_path)
}

/// Run the generated script interactively with standard terminal I/O.
/// Allows sudo, pacman, yay, and user prompts to display in real-time.
pub fn run_interactive(script: &Path, auto_confirm: Option<bool>) -> Result<ExitStatus> {
    let mut cmd = Command::new("bash");
    cmd.arg(script);

    // If auto_confirm is Some(true), pass --noconfirm for pacman/yay
    if auto_confirm == Some(true) {
        cmd.arg("--noconfirm");
    }

    cmd.stdin(Stdio::inherit());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());

    let status = cmd
        .status()
        .with_context(|| format!("Failed to execute script at {}", script.display()))?;

    Ok(status)
}

/// Clean up the temp script file.
pub fn cleanup(path: &Path) {
    let _ = fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn get_update_script_path() -> String {
        let candidate = Path::new("../update.sh");
        if candidate.exists() {
            return candidate.to_string_lossy().into_owned();
        }
        let candidate2 = Path::new("update.sh");
        if candidate2.exists() {
            return candidate2.to_string_lossy().into_owned();
        }
        "../update.sh".to_string()
    }

    #[test]
    fn test_generate_script_all_steps_syntax() {
        let update_path = get_update_script_path();
        if !Path::new(&update_path).exists() {
            return;
        }

        let all_steps = Step::all();
        let selected: Vec<&Step> = all_steps.iter().collect();

        for confirm in [None, Some(true), Some(false)] {
            let script_path = generate_script(&update_path, &selected, confirm).unwrap();
            let status = Command::new("bash")
                .arg("-n")
                .arg(&script_path)
                .status()
                .expect("Failed to run bash -n");

            cleanup(&script_path);
            assert!(status.success(), "Generated script failed bash syntax check");
        }
    }

    #[test]
    fn test_generate_script_individual_steps_syntax() {
        let update_path = get_update_script_path();
        if !Path::new(&update_path).exists() {
            return;
        }

        let all_steps = Step::all();
        for step in &all_steps {
            let selected = vec![step];
            let script_path = generate_script(&update_path, &selected, None).unwrap();
            let status = Command::new("bash")
                .arg("-n")
                .arg(&script_path)
                .status()
                .expect("Failed to run bash -n");

            cleanup(&script_path);
            assert!(
                status.success(),
                "Step '{}' alone failed bash syntax check",
                step.name
            );
        }
    }
}

