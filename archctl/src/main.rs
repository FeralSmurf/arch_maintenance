mod journal;
mod runner;
mod steps;
mod tui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "archctl",
    about = "Interactive TUI frontend for Arch Linux system maintenance and update.sh",
    version
)]
struct Cli {
    /// Path to update.sh (defaults to searching current directory, parents, and $HOME)
    #[arg(default_value = "update.sh")]
    script: String,

    /// Auto-confirm all prompts (yes)
    #[arg(short = 'y', long = "yes")]
    auto_yes: bool,

    /// Auto-decline all prompts (no)
    #[arg(short = 'n', long = "no")]
    auto_no: bool,

    /// Show what would be executed without running
    #[arg(short = 'd', long = "dry-run")]
    dry_run: bool,
}

fn find_update_script() -> Option<String> {
    // 1. Check current directory and parents
    if let Ok(mut dir) = std::env::current_dir() {
        loop {
            let candidate = dir.join("update.sh");
            if candidate.exists() {
                return Some(candidate.to_string_lossy().into_owned());
            }
            if !dir.pop() {
                break;
            }
        }
    }
    // 2. Check HOME directory
    if let Ok(home) = std::env::var("HOME") {
        let candidate = std::path::Path::new(&home).join("update.sh");
        if candidate.exists() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let script_path = if cli.script != "update.sh" {
        cli.script
    } else {
        match find_update_script() {
            Some(path) => path,
            None => {
                anyhow::bail!(
                    "Could not find 'update.sh' in the current directory, parent directories, or $HOME.\n\
                     Please specify the path directly:\n    archctl /path/to/update.sh"
                );
            }
        }
    };

    if !std::path::Path::new(&script_path).exists() {
        anyhow::bail!("Script file does not exist: {}", script_path);
    }

    let auto_confirm = if cli.auto_yes {
        Some(true)
    } else if cli.auto_no {
        Some(false)
    } else {
        None
    };

    tui::run(&script_path, auto_confirm, cli.dry_run)
}
