#!/usr/bin/env bash
# ==============================================================================
# Arch Linux System Update & Maintenance
# ==============================================================================
set -uo pipefail

# ------------------------------------------------------------------------------
# Colors & Formatting
# ------------------------------------------------------------------------------
if [[ -t 1 ]] && [[ -z "${NO_COLOR:-}" ]]; then
    BOLD="\033[1m"
    DIM="\033[2m"
    RESET="\033[0m"

    # Standard colors
    RED="\033[31m"
    GREEN="\033[32m"
    YELLOW="\033[33m"
    BLUE="\033[34m"
    MAGENTA="\033[35m"
    CYAN="\033[36m"
    WHITE="\033[37m"

    # Bold colors
    B_RED="\033[1;31m"
    B_GREEN="\033[1;32m"
    B_YELLOW="\033[1;33m"
    B_BLUE="\033[1;34m"
    B_MAGENTA="\033[1;35m"
    B_CYAN="\033[1;36m"
    B_WHITE="\033[1;37m"
else
    BOLD=""
    DIM=""
    RESET=""
    RED=""
    GREEN=""
    YELLOW=""
    BLUE=""
    MAGENTA=""
    CYAN=""
    WHITE=""
    B_RED=""
    B_GREEN=""
    B_YELLOW=""
    B_BLUE=""
    B_MAGENTA=""
    B_CYAN=""
    B_WHITE=""
fi

# ------------------------------------------------------------------------------
# Helper Print Functions
# ------------------------------------------------------------------------------
banner() {
    echo -e "${B_CYAN}  ┌────────────────────────────────────────────────────────┐${RESET}"
    echo -e "${B_CYAN}  │       ${B_WHITE}ARCH LINUX SYSTEM UPGRADE & MAINTENANCE${B_CYAN}          │${RESET}"
    echo -e "${B_CYAN}  └────────────────────────────────────────────────────────┘${RESET}"
}

section() {
    echo
    echo -e "${B_CYAN}━━━┫ ${B_WHITE}$1 ${B_CYAN}┣━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
}

info() {
    echo -e "${B_BLUE}[ℹ]${RESET} $*"
}

success() {
    echo -e "${B_GREEN}[✓]${RESET} $*"
}

warn() {
    echo -e "${B_YELLOW}[!]${RESET} $*"
}

error() {
    echo -e "${B_RED}[✗]${RESET} $*"
}

step() {
    echo -e "${B_MAGENTA}::${RESET} ${B_WHITE}$*${RESET}"
}

delete_trash() {
    echo
    step "Checking user trash..."
    local trash_dir="${XDG_DATA_HOME:-$HOME/.local/share}/Trash"
    local item_count=0
    local trash_size="0B"

    if [[ -d "$trash_dir" ]]; then
        trash_size=$(du -sh "$trash_dir" 2>/dev/null | cut -f1)
    fi

    if [[ -d "$trash_dir/files" ]]; then
        item_count=$(find "$trash_dir/files" -mindepth 1 -maxdepth 1 2>/dev/null | wc -l)
    fi

    if [[ "$item_count" -gt 0 ]]; then
        warn "Trash contains ${item_count} item(s) (${trash_size})."
        read -r -p "Delete all items in trash? [y/N]: " clean_trash
        if [[ "${clean_trash:-}" =~ ^[Yy]$ ]]; then
            if command -v gio >/dev/null 2>&1; then
                gio trash --empty
            elif command -v trash-empty >/dev/null 2>&1; then
                trash-empty -f
            else
                rm -rf "${trash_dir}/files/"* "${trash_dir}/info/"*
            fi
            success "Trash emptied successfully."
        else
            info "Skipped trash cleanup."
        fi
    else
        success "Trash is already empty."
    fi
}

check_broken_symlinks() {
    echo
    step "Checking for broken symbolic links in $HOME..."
    local search_dir="$HOME"
    local broken_links=()

    # Search for broken symlinks while pruning cache, trash, and runtime containers (Steam, Wine)
    while IFS= read -r link; do
        [[ -n "$link" ]] && broken_links+=("$link")
    done < <(find "$search_dir" -xdev \
        \( -path "$search_dir/.cache" \
        -o -path "$search_dir/.local/share/Trash" \
        -o -path "$search_dir/.local/share/Steam" \
        -o -path "$search_dir/.steam" \
        -o -path "$search_dir/.wine" \
        -o -path "$search_dir/.var" \
        -o -path "*/.mozilla/*" \
        -o -path "*/.thunderbird/*" \) -prune \
        -o -xtype l -print 2>/dev/null || true)

    if [[ ${#broken_links[@]} -gt 0 ]]; then
        warn "Found ${#broken_links[@]} broken symbolic link(s):"
        for link in "${broken_links[@]}"; do
            local target
            target=$(readlink "$link" 2>/dev/null || echo "nonexistent")
            echo -e "    ${B_YELLOW}•${RESET} ${link} ${DIM}➜ ${target}${RESET}"
        done
        echo
        echo -e "    ${DIM}Note: some broken symlinks are intentional; review the list before deleting.${RESET}"
        read -r -p "Delete these broken symlinks? [y/N]: " clean_syms
        if [[ "${clean_syms:-}" =~ ^[Yy]$ ]]; then
            local count=0
            for link in "${broken_links[@]}"; do
                if rm -f "$link"; then
                    ((count++))
                fi
            done
            success "Removed ${count} broken symlink(s)."
        else
            info "Skipped broken symlink removal."
        fi
    else
        success "No broken symbolic links found in $HOME."
    fi
}

# ------------------------------------------------------------------------------
# Pre-flight Checks
# ------------------------------------------------------------------------------
banner

# 1. Determine package manager (yay -> paru -> pacman)
if command -v yay >/dev/null 2>&1; then
    HELPER="yay"
elif command -v paru >/dev/null 2>&1; then
    HELPER="paru"
elif command -v pacman >/dev/null 2>&1; then
    HELPER="sudo pacman"
else
    error "No supported package manager found (yay, paru, or pacman)."
    exit 1
fi
info "Using package manager: ${B_WHITE}${HELPER}${RESET}"

# 2. Check for stale pacman database lock
PACMAN_DB_LCK="/var/lib/pacman/db.lck"
if [[ -f "$PACMAN_DB_LCK" ]]; then
    warn "Pacman database lock exists: ${PACMAN_DB_LCK}"
    if pgrep -x "pacman" >/dev/null 2>&1 || pgrep -x "yay" >/dev/null 2>&1 || pgrep -x "paru" >/dev/null 2>&1; then
        error "Another package manager process is currently active. Aborting."
        exit 1
    else
        warn "No active package manager detected. The lock may be stale."
        read -r -p "Remove the stale lock file and continue? [y/N]: " rm_lck
        if [[ "${rm_lck:-}" =~ ^[Yy]$ ]]; then
            sudo rm -f "$PACMAN_DB_LCK"
            success "Stale lock removed."
        else
            error "Cannot proceed while db.lck is present."
            exit 1
        fi
    fi
fi

# 3. Track current line in pacman.log to precisely inspect this upgrade run
PACMAN_LOG="/var/log/pacman.log"
LOG_START_LINE=0
if [[ -f "$PACMAN_LOG" ]]; then
    LOG_START_LINE=$(wc -l < "$PACMAN_LOG")
fi

# ------------------------------------------------------------------------------
# System Update
# ------------------------------------------------------------------------------
section "Package Upgrade"
step "Upgrading system and AUR packages with ${HELPER}..."

if ! $HELPER -Syu "$@"; then
    echo
    error "Update failed! Please review the error messages above."
    exit 1
fi
echo
success "System packages updated successfully."

# ------------------------------------------------------------------------------
# Post-Upgrade Maintenance
# ------------------------------------------------------------------------------
section "Maintenance & Diagnostics"

# 1. Check for .pacnew and .pacsave files
step "Checking for configuration files (.pacnew / .pacsave)..."
PACNEW_FILES=()
if command -v pacdiff >/dev/null 2>&1; then
    while IFS= read -r f; do
        [[ -n "$f" ]] && PACNEW_FILES+=("$f")
    done < <(pacdiff -o 2>/dev/null || true)
else
    while IFS= read -r f; do
        [[ -n "$f" ]] && PACNEW_FILES+=("$f")
    done < <(find /etc -regextype posix-extended -regex ".+\.pac(new|save)" 2>/dev/null || true)
fi

if [[ ${#PACNEW_FILES[@]} -gt 0 ]]; then
    warn "Found ${#PACNEW_FILES[@]} configuration file(s) requiring review:"
    for f in "${PACNEW_FILES[@]}"; do
        echo -e "    ${B_YELLOW}•${RESET} $f"
    done
    echo -e "    ${DIM}Hint: Run 'sudo pacdiff' or 'DIFFPROG=meld pacdiff' to merge changes.${RESET}"
else
    success "No .pacnew or .pacsave files detected."
fi

# 2. Check for orphaned packages
echo
step "Checking for unneeded orphaned dependencies..."
ORPHANS=$(pacman -Qtdq 2>/dev/null || true)
if [[ -n "$ORPHANS" ]]; then
    ORPHAN_COUNT=$(echo "$ORPHANS" | wc -w)
    warn "Found $ORPHAN_COUNT orphaned package(s):"
    echo "$ORPHANS" | tr '\n' ' ' | fold -s -w 70 | sed 's/^/    /'
    echo
    read -r -p "Remove these orphaned packages? [y/N]: " clean_orphans
    if [[ "${clean_orphans:-}" =~ ^[Yy]$ ]]; then
        # shellcheck disable=SC2086
        sudo pacman -Rns $ORPHANS
        success "Orphaned packages removed."
    else
        info "Skipped orphan removal."
    fi
else
    success "No orphaned packages found."
fi

# 3. Pacman cache maintenance
if command -v paccache >/dev/null 2>&1; then
    echo
    step "Checking pacman cache size..."
    CACHE_CANDIDATES=$(paccache -d -k2 2>/dev/null | grep -E "candidates" || true)
    if [[ -n "$CACHE_CANDIDATES" ]]; then
        info "$CACHE_CANDIDATES"
        read -r -p "Clean package cache (keep last 2 versions)? [y/N]: " clean_cache
        if [[ "${clean_cache:-}" =~ ^[Yy]$ ]]; then
            sudo paccache -rk2
            sudo paccache -ruk0
            success "Pacman cache cleaned."
        else
            info "Skipped cache cleanup."
        fi
    fi
fi

# 4. Empty user trash
delete_trash

# 5. Check for broken symbolic links
check_broken_symlinks

# 6. Failed systemd services check
echo
step "Checking systemd service health..."
FAILED_ANY=0
FAILED_SERVICES=$(systemctl --failed --no-legend --plain 2>/dev/null || true)
if [[ -n "$FAILED_SERVICES" ]]; then
    FAILED_ANY=1
    warn "Detected failed system service(s):"
    echo "$FAILED_SERVICES" | sed 's/^/    /'
fi
FAILED_USER_SERVICES=""
if [[ -n "${XDG_RUNTIME_DIR:-}" ]]; then
    FAILED_USER_SERVICES=$(systemctl --user --failed --no-legend --plain 2>/dev/null || true)
fi
if [[ -n "$FAILED_USER_SERVICES" ]]; then
    FAILED_ANY=1
    warn "Detected failed user service(s):"
    echo "$FAILED_USER_SERVICES" | sed 's/^/    /'
fi
if [[ $FAILED_ANY -eq 0 ]]; then
    success "All systemd units are running normally."
else
    echo -e "    ${DIM}Hint: Inspect with 'systemctl status <service>' or 'journalctl -xe'${RESET}"
fi

# ------------------------------------------------------------------------------
# Reboot Detection
# ------------------------------------------------------------------------------
section "Reboot Status"

REBOOT_NEEDED=0
REBOOT_REASONS=()

# Check A: Running kernel modules directory exists
RUNNING_KERNEL=$(uname -r)
if [[ -d "/usr/lib/modules" && ! -d "/usr/lib/modules/${RUNNING_KERNEL}" ]]; then
    REBOOT_NEEDED=1
    REBOOT_REASONS+=("Running kernel (${RUNNING_KERNEL}) was replaced by a newer kernel")
fi

# Check B: Packages that require a reboot, upgraded since the last boot.
# Kernel packages are matched against the running kernel's pkgbase, so upgrading
# "linux" while booted into "linux-lts" no longer flags a reboot.
RUNNING_PKGBASE=$(cat "/usr/lib/modules/${RUNNING_KERNEL}/pkgbase" 2>/dev/null || true)
[[ -z "$RUNNING_PKGBASE" ]] && RUNNING_PKGBASE="linux"
REBOOT_REGEX="^(${RUNNING_PKGBASE}|systemd|systemd-libs|systemd-sysvcompat|udev|glibc|linux-firmware|amd-ucode|intel-ucode|nvidia|nvidia-dkms|nvidia-open|nvidia-open-dkms|cryptsetup)$"

# Scan since boot so this step works on its own. Fall back to the line offset
# recorded by the pre-flight step when the boot time is unavailable.
BOOT_EPOCH=0
BOOT_TIME=$(uptime -s 2>/dev/null || true)
if [[ -n "$BOOT_TIME" ]]; then
    BOOT_EPOCH=$(date -d "$BOOT_TIME" +%s 2>/dev/null || echo 0)
fi

if [[ -f "$PACMAN_LOG" ]]; then
    if [[ "$BOOT_EPOCH" -gt 0 ]]; then
        PACMAN_SLICE=$(grep -E '\[ALPM\] (upgraded|installed) ' "$PACMAN_LOG" 2>/dev/null | tail -n 2000 || true)
    else
        PACMAN_SLICE=$(tail -n +"$(( ${LOG_START_LINE:-0} + 1 ))" "$PACMAN_LOG" 2>/dev/null | grep -E '\[ALPM\] (upgraded|installed) ' || true)
    fi

    if [[ -n "$PACMAN_SLICE" ]]; then
        UPGRADED_CRITICAL=$(
            while IFS= read -r entry; do
                pkg=$(sed -E 's/.*\[ALPM\] (upgraded|installed) ([^ ]+) .*/\2/' <<< "$entry")
                [[ "$pkg" =~ $REBOOT_REGEX ]] || continue
                if [[ "$BOOT_EPOCH" -gt 0 ]]; then
                    entry_ts=${entry%%]*}
                    entry_ts=${entry_ts#[}
                    entry_epoch=$(date -d "$entry_ts" +%s 2>/dev/null || echo 0)
                    [[ "$entry_epoch" -gt 0 && "$entry_epoch" -lt "$BOOT_EPOCH" ]] && continue
                fi
                echo "$pkg"
            done <<< "$PACMAN_SLICE" | sort -u
        )
        if [[ -n "$UPGRADED_CRITICAL" ]]; then
            REBOOT_NEEDED=1
            while IFS= read -r pkg; do
                [[ -n "$pkg" ]] && REBOOT_REASONS+=("Core package updated: ${pkg}")
            done <<< "$UPGRADED_CRITICAL"
        fi
    fi
fi

if [[ $REBOOT_NEEDED -eq 1 ]]; then
    warn "A reboot is recommended for system changes to take effect:"
    for reason in "${REBOOT_REASONS[@]}"; do
        echo -e "    ${B_YELLOW}➜${RESET} ${reason}"
    done
    echo
    read -r -p "Reboot system now? [y/N]: " reboot_ans
    case "${reboot_ans:-}" in
        y|Y|yes|YES)
            step "Rebooting system..."
            systemctl reboot
            ;;
        *)
            echo -e "${B_YELLOW}[!] Reboot postponed.${RESET}"
            ;;
    esac
else
    success "System is fully up to date! No reboot required."
fi

echo
