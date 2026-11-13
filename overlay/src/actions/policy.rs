//! Which level an action gets (docs/overlay.md "Actions"):
//! - level 0 runs at once (an Undo chip where it can be undone);
//! - level 1 waits for Enter on the amber card (a spoken "yes" never counts);
//! - level 2 never runs: packages, sudo/pkexec, deleting outside the trash, autostart and shell
//!   profiles, secrets. `run_command`'s argv is checked for all of these too.
//!
//! A tainted turn (outside text came in: a weather result in the follow-up's context, or
//! pasted text) and everything after a weather lookup in the same answer need Enter as well;
//! `confirm_all` makes every action wait for Enter (read-only weather lookups excepted).

use super::schema::Action;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Level {
    Run,
    Confirm,
    /// Never: why (English key, translated when shown).
    Blocked(String),
}

impl Level {
    pub fn number(&self) -> u8 {
        match self {
            Level::Run => 0,
            Level::Confirm => 1,
            Level::Blocked(_) => 2,
        }
    }
}

/// Reasons (translation keys).
pub const WHY_PACKAGES: &str = "Installing or removing software is never done from here.";
pub const WHY_ADMIN: &str = "Administrator rights are never used from here.";
pub const WHY_DELETE: &str = "Deleting files outside the trash is never done from here.";
pub const WHY_AUTOSTART: &str = "Autostart and shell start files are never changed from here.";
pub const WHY_SECRETS: &str = "Keys, passwords and secret files are never touched from here.";
pub const WHY_SHELL: &str = "A shell or interpreter would hide what really runs.";
pub const WHY_SYSTEM: &str = "Disks and system files are never changed from here.";

fn base(program: &str) -> &str {
    program.rsplit('/').next().unwrap_or(program)
}

/// Wrappers that run the rest of the argv as the real command.
fn wrapper_skip(argv: &[String]) -> Option<usize> {
    let mut i = 1;
    match base(&argv[0]) {
        "env" => {
            while i < argv.len() && (argv[i].starts_with('-') || argv[i].contains('=')) {
                i += 1;
            }
        }
        "nice" | "ionice" | "nohup" | "setsid" | "stdbuf" | "time" | "chrt" | "taskset" | "flock" | "unbuffer" | "systemd-run" | "uwsm" | "uwsm-app" | "app2unit" => {
            // their options, and a value right after an option (nice -n 5, timeout 10 …)
            while i < argv.len() && argv[i].starts_with('-') {
                i += 1;
                if i < argv.len() && argv[i].chars().all(|c| c.is_ascii_digit()) {
                    i += 1;
                }
            }
            if base(&argv[0]) == "uwsm" && argv.get(i).is_some_and(|a| a == "app") {
                i += 1;
                while i < argv.len() && argv[i].starts_with('-') && argv[i] != "--" {
                    i += 1;
                }
            }
            if argv.get(i).is_some_and(|a| a == "--") {
                i += 1;
            }
        }
        "timeout" => {
            while i < argv.len() && argv[i].starts_with('-') {
                i += 1;
            }
            i += 1; // the duration
        }
        _ => return None,
    }
    Some(i)
}

fn has(argv: &[String], words: &[&str]) -> bool {
    argv.iter().skip(1).any(|a| words.contains(&a.as_str()))
}

fn secret_path(arg: &str) -> bool {
    let a = arg.to_ascii_lowercase();
    let file = a.rsplit('/').next().unwrap_or(&a);
    [".ssh", ".gnupg", "keyrings", "kwalletd", ".password-store", ".aws", ".netrc", ".git-credentials", ".pgpass", "/etc/shadow", "/etc/gshadow", "/etc/sudoers", ".config/gh/hosts", ".docker/config.json", ".kube/config", "id_rsa", "id_ed25519", "id_ecdsa"]
        .iter()
        .any(|s| a.contains(s))
        || file == ".env"
        || file.starts_with(".env.")
        || file.ends_with(".env")
        || file.ends_with(".pem")
        || file.ends_with(".key")
}

fn startup_path(arg: &str) -> bool {
    let a = arg.to_ascii_lowercase();
    let file = a.rsplit('/').next().unwrap_or(&a);
    ["autostart", ".config/systemd", "environment.d", ".config/fish", "/etc/profile", "/etc/xdg", "plasma-workspace/env", "plasma-workspace/shutdown", "cron"].iter().any(|s| a.contains(s))
        || [".bashrc", ".bash_profile", ".bash_login", ".bash_logout", ".profile", ".zshrc", ".zshenv", ".zprofile", ".zlogin", ".xprofile", ".xinitrc", ".xsession", ".pam_environment", "config.fish"].contains(&file)
}

/// Why this argv must never run (None: it may, after Enter).
pub fn blocked_argv(argv: &[String]) -> Option<&'static str> {
    if argv.is_empty() {
        return Some(WHY_SHELL);
    }
    if let Some(skip) = wrapper_skip(argv) {
        // `env`, `nice`, `timeout` … : judge the command they run (and the wrapper's own args).
        if let Some(why) = argv[1..skip.min(argv.len())].iter().find_map(|a| if secret_path(a) { Some(WHY_SECRETS) } else if startup_path(a) { Some(WHY_AUTOSTART) } else { None }) {
            return Some(why);
        }
        return if skip >= argv.len() { None } else { blocked_argv(&argv[skip..]) };
    }
    let program = base(&argv[0]).to_ascii_lowercase();
    let p = program.as_str();
    if ["sudo", "doas", "pkexec", "su", "run0", "sudoedit", "polkit-agent-helper-1", "machinectl", "chpasswd", "passwd", "usermod", "useradd", "userdel", "groupadd", "visudo"].contains(&p) {
        return Some(WHY_ADMIN);
    }
    if ["pacman", "yay", "paru", "pamac", "pikaur", "trizen", "aura", "makepkg", "apt", "apt-get", "aptitude", "dpkg", "dnf", "yum", "rpm", "zypper", "snap", "emerge", "xbps-install", "xbps-remove", "nix-env", "brew", "pkcon", "pkgctl", "appimagelauncher", "omarchy-pkg-add", "omarchy-pkg-drop", "omarchy-pkg-install", "omarchy-pkg-remove"].contains(&p)
        || (p == "flatpak" && has(argv, &["install", "uninstall", "remove", "update", "upgrade", "repair", "remote-add", "remote-delete", "override"]))
        || (["pip", "pip3", "pipx", "uv", "gem", "go"].contains(&p) && has(argv, &["install", "uninstall", "remove", "add", "upgrade"]))
        || (p == "cargo" && has(argv, &["install", "uninstall"]))
        || (["npm", "pnpm", "yarn", "bun"].contains(&p) && (has(argv, &["-g", "--global", "global"])))
        || p == "maprisma-manager-core"
    {
        return Some(WHY_PACKAGES);
    }
    if ["rm", "rmdir", "unlink", "shred", "srm", "wipe", "truncate"].contains(&p) || (p == "find" && has(argv, &["-delete", "-exec", "-execdir", "-ok", "-okdir"])) || (p == "git" && has(argv, &["clean"])) || (p == "rsync" && argv.iter().any(|a| a.starts_with("--delete") || a == "--remove-source-files")) {
        return Some(WHY_DELETE);
    }
    if p.starts_with("mkfs") || ["dd", "wipefs", "fdisk", "sfdisk", "cfdisk", "parted", "gdisk", "sgdisk", "mkswap", "cryptsetup", "mount", "umount", "losetup", "blkdiscard", "grub-install", "grub-mkconfig", "mkinitcpio", "bootctl", "efibootmgr", "modprobe", "rmmod", "insmod", "sysctl", "reboot", "poweroff", "shutdown", "halt"].contains(&p) {
        return Some(WHY_SYSTEM);
    }
    if p == "systemctl" && has(argv, &["enable", "disable", "mask", "unmask", "link", "preset", "preset-all", "edit", "revert", "set-default", "reboot", "poweroff", "halt", "suspend", "hibernate", "kexec"]) {
        return Some(if has(argv, &["reboot", "poweroff", "halt", "suspend", "hibernate", "kexec"]) { WHY_SYSTEM } else { WHY_AUTOSTART });
    }
    if ["crontab", "at", "batch"].contains(&p) || p == "kwriteconfig6" && argv.iter().any(|a| a.contains("autostart")) {
        return Some(WHY_AUTOSTART);
    }
    if ["secret-tool", "pass", "gpg", "gpg2", "gpg-agent", "ssh-add", "ssh-keygen", "kwallet-query", "kwalletcli", "keyring", "gnome-keyring-daemon", "1password", "op", "bw", "keepassxc-cli", "lpass"].contains(&p) {
        return Some(WHY_SECRETS);
    }
    // Shells and interpreters with code on the command line, and other ways to run hidden code.
    let shells = ["sh", "bash", "zsh", "fish", "dash", "ksh", "tcsh", "csh", "nu", "elvish", "xonsh", "busybox"];
    let interpreters = ["python", "python3", "python2", "perl", "ruby", "node", "nodejs", "deno", "bun", "php", "lua", "luajit", "tclsh", "osascript", "awk", "gawk", "mawk", "nawk", "dart", "qjs", "julia", "r", "rscript"];
    let plain_py = p.starts_with("python3.") || p.starts_with("perl5");
    if shells.contains(&p) || ["eval", "exec", "xargs", "parallel", "script", "expect", "watch", "nsenter", "unshare", "bwrap", "firejail", "chroot", "strace", "ltrace", "gdb", "screen", "tmux", "ssh", "scp", "sftp"].contains(&p) {
        return Some(WHY_SHELL);
    }
    if interpreters.contains(&p) || plain_py {
        return Some(WHY_SHELL);
    }
    if ["hyprctl", "kwin", "qdbus", "qdbus6", "dbus-send", "busctl", "gdbus"].contains(&p) && argv.iter().any(|a| a.contains("exec") || a.contains("Exec") || a.contains("loadScript") || a.contains("eval") || a.contains("keyword")) {
        return Some(WHY_SHELL);
    }
    for a in &argv[1..] {
        if secret_path(a) {
            return Some(WHY_SECRETS);
        }
        if startup_path(a) {
            return Some(WHY_AUTOSTART);
        }
    }
    if secret_path(&argv[0]) {
        return Some(WHY_SECRETS);
    }
    None
}

/// The level before taint and `confirm_all`.
pub fn base_level(action: &Action) -> Level {
    match action {
        Action::OpenApp { .. } | Action::OpenUrl { .. } | Action::Dnd { .. } | Action::ThemeMode { .. } | Action::Volume { .. } | Action::Media { .. } | Action::Workspace { .. } | Action::MoveWindow { .. } | Action::Weather { .. } => Level::Run,
        Action::CloseWindow => Level::Confirm,
        Action::RunCommand { argv, .. } => match blocked_argv(argv) {
            Some(why) => Level::Blocked(why.into()),
            None => Level::Confirm,
        },
    }
}

/// Read-only: never changes anything, so neither taint nor `confirm_all` hold it back.
pub fn read_only(action: &Action) -> bool {
    matches!(action, Action::Weather { .. })
}

/// The levels of one answer's actions, in order. `tainted`: the turn already carries outside text.
pub fn classify(actions: &[Action], tainted: bool, confirm_all: bool) -> Vec<Level> {
    let mut after_outside = tainted;
    actions
        .iter()
        .map(|a| {
            let mut level = base_level(a);
            if level == Level::Run && !read_only(a) && (after_outside || confirm_all) {
                level = Level::Confirm;
            }
            if read_only(a) {
                after_outside = true;
            }
            level
        })
        .collect()
}

/// Text that came from somewhere else: pasted (several lines, or very long).
pub fn looks_pasted(question: &str) -> bool {
    question.trim().lines().count() > 2 || question.chars().count() > 600
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::schema::{Media, ThemeMode};

    fn argv(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn levels() {
        assert_eq!(base_level(&Action::OpenApp { name: "x".into() }), Level::Run);
        assert_eq!(base_level(&Action::Media { command: Media::Next }), Level::Run);
        assert_eq!(base_level(&Action::CloseWindow), Level::Confirm);
        assert_eq!(base_level(&Action::RunCommand { argv: argv("uname -r"), why: String::new() }), Level::Confirm);
        assert_eq!(base_level(&Action::RunCommand { argv: argv("sudo ls"), why: String::new() }), Level::Blocked(WHY_ADMIN.into()));
        assert_eq!(Level::Blocked("x".into()).number(), 2);
    }

    #[test]
    fn block_list() {
        for (cmd, why) in [
            ("sudo pacman -Syu", WHY_ADMIN),
            ("/usr/bin/pkexec true", WHY_ADMIN),
            ("pacman -S vim", WHY_PACKAGES),
            ("yay -Rns foo", WHY_PACKAGES),
            ("flatpak install flathub x", WHY_PACKAGES),
            ("pip install requests", WHY_PACKAGES),
            ("npm i -g x", WHY_PACKAGES),
            ("rm -rf /tmp/x", WHY_DELETE),
            ("find . -name x -delete", WHY_DELETE),
            ("find . -exec rm {} ;", WHY_DELETE),
            ("env FOO=1 rm x", WHY_DELETE),
            ("nice -n 5 sudo x", WHY_ADMIN),
            ("timeout 5 pacman -Q", WHY_PACKAGES),
            ("setsid -f yay", WHY_PACKAGES),
            ("cat /home/u/.ssh/id_ed25519", WHY_SECRETS),
            ("cp project/.env /tmp/", WHY_SECRETS),
            ("ls /home/u/.local/share/keyrings", WHY_SECRETS),
            ("gpg --export-secret-keys", WHY_SECRETS),
            ("secret-tool lookup x y", WHY_SECRETS),
            ("cp x /home/u/.config/autostart/x.desktop", WHY_AUTOSTART),
            ("tee -a /home/u/.bashrc", WHY_AUTOSTART),
            ("systemctl --user enable foo", WHY_AUTOSTART),
            ("crontab -e", WHY_AUTOSTART),
            ("bash -c ls", WHY_SHELL),
            ("sh", WHY_SHELL),
            ("python3 -c print(1)", WHY_SHELL),
            ("xargs rm", WHY_SHELL),
            ("hyprctl dispatch exec kitty", WHY_SHELL),
            ("dd if=/dev/zero of=/dev/sda", WHY_SYSTEM),
            ("mkfs.ext4 /dev/sdb1", WHY_SYSTEM),
            ("systemctl reboot", WHY_SYSTEM),
        ] {
            assert_eq!(blocked_argv(&argv(cmd)), Some(why), "{cmd}");
        }
        for ok in ["uname -r", "ls -la /tmp", "df -h", "flatpak list", "pacman-like", "git status", "systemctl --user status pipewire", "env", "gio trash x", "hyprctl clients", "cargo build", "npm install"] {
            assert_eq!(blocked_argv(&argv(ok)), None, "{ok}");
        }
    }

    #[test]
    fn taint_and_confirm_all() {
        let weather = Action::Weather { location: "Berlin".into() };
        let open = Action::OpenApp { name: "x".into() };
        let dark = Action::ThemeMode { mode: ThemeMode::Dark };
        assert_eq!(classify(&[open.clone(), dark.clone()], false, false), vec![Level::Run, Level::Run]);
        assert_eq!(classify(&[open.clone()], true, false), vec![Level::Confirm]);
        assert_eq!(classify(&[weather.clone()], true, false), vec![Level::Run]);
        // after a weather lookup in the same answer
        assert_eq!(classify(&[open.clone(), weather.clone(), dark.clone()], false, false), vec![Level::Run, Level::Run, Level::Confirm]);
        assert_eq!(classify(&[open.clone(), weather.clone()], false, true), vec![Level::Confirm, Level::Run]);
        let blocked = Action::RunCommand { argv: argv("sudo x"), why: String::new() };
        assert!(matches!(classify(&[blocked], true, true)[0], Level::Blocked(_)));
        assert!(looks_pasted("a\nb\nc"));
        assert!(looks_pasted(&"x".repeat(601)));
        assert!(!looks_pasted("open firefox"));
    }
}
