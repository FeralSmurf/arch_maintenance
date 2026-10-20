#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    PreFlight,
    Upgrade,
    Maintenance,
    Reboot,
}

#[derive(Debug, Clone)]
pub struct Step {
    pub name: &'static str,
    pub description: &'static str,
    pub category: Category,
}

impl Step {
    pub fn all() -> Vec<Self> {
        vec![
            Step {
                name: "Pre-flight checks",
                description: "Detect package manager, check db.lck, track log position",
                category: Category::PreFlight,
            },
            Step {
                name: "System upgrade",
                description: "Run pacman -Syu with detected helper (yay/paru/pacman)",
                category: Category::Upgrade,
            },
            Step {
                name: "Flatpak update",
                description: "Update Flatpak packages if flatpak is installed",
                category: Category::Upgrade,
            },
            Step {
                name: "Pacnew/pacsave check",
                description: "Find configuration files requiring review",
                category: Category::Maintenance,
            },
            Step {
                name: "Orphaned packages",
                description: "Find and optionally remove unneeded dependencies",
                category: Category::Maintenance,
            },
            Step {
                name: "Pacman cache cleanup",
                description: "Clean package cache (keep last 2 versions)",
                category: Category::Maintenance,
            },
            Step {
                name: "Trash cleanup",
                description: "Empty user trash directory",
                category: Category::Maintenance,
            },
            Step {
                name: "Broken symlinks",
                description: "Find and optionally remove broken symlinks in $HOME",
                category: Category::Maintenance,
            },
            Step {
                name: "Failed systemd services",
                description: "Check for failed systemd units",
                category: Category::Maintenance,
            },
            Step {
                name: "Reboot detection",
                description: "Check if reboot is needed after kernel/core package updates",
                category: Category::Reboot,
            },
        ]
    }
}
