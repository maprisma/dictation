//! Which level an action gets (docs/overlay.md "Actions"):
//! - level 0 runs at once (an Undo chip where it can be undone);
//! - level 1 waits for Enter on the amber card (a spoken "yes" never counts);
//! - level 2 never runs: packages, sudo/pkexec, deleting outside the trash, autostart and shell
//!   profiles, secrets. `run_command`'s argv is checked for all of these too.
//!
//! A tainted turn (outside text came in: a weather result or command output in the follow-up's
//! context, an earlier tainted exchange in the chain, or pasted text) and everything after a weather lookup in the same answer need Enter as well;
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

/// Wrappers that run (part of) the rest of their argv as the real command. Their options are
/// not parsed: which argument is the program depends on each option's value, so every suffix of
/// the argv is judged instead (fail closed).
const WRAPPERS: &[&str] = &[
    "env", "nice", "ionice", "nohup", "setsid", "stdbuf", "time", "chrt", "taskset", "flock", "unbuffer", "systemd-run", "systemd-inhibit", "uwsm", "uwsm-app", "app2unit", "timeout", "prlimit", "numactl", "dbus-run-session", "dbus-launch", "gamemoderun", "prime-run", "mangohud", "cgexec",
];

fn is_wrapper(program: &str) -> bool {
    WRAPPERS.contains(&base(program).to_ascii_lowercase().as_str())
}

/// A wrapper option that itself turns text into a command line (`env -S "rm x"`), or a unit
/// property that runs commands (`systemd-run -p ExecStartPre=…`).
fn wrapper_runs_text(argv: &[String]) -> bool {
    match base(&argv[0]).to_ascii_lowercase().as_str() {
        "env" => argv[1..].iter().any(|a| {
            a.starts_with("--split-string") || (a.starts_with('-') && !a.starts_with("--") && a[1..].contains('S'))
        }),
        "systemd-run" => argv[1..].iter().any(|a| a.contains("Exec")),
        _ => false,
    }
}

/// Terminal emulators: plain "open a terminal" is fine, but any way to make them run a command
/// (`-e`, `--command`, `--`, a trailing command, a shell/program setting) is a shell.
const TERMINALS: &[&str] = &[
    "kitty", "kitten", "alacritty", "konsole", "foot", "footclient", "wezterm", "wezterm-gui", "ghostty", "gnome-terminal", "kgx", "ptyxis", "cosmic-term", "xterm", "uxterm", "urxvt", "rxvt", "st", "xfce4-terminal", "tilix", "terminator", "lxterminal", "qterminal", "terminology", "rio", "contour", "tilda", "yakuake", "xdg-terminal-exec",
];

fn terminal_runs_command(argv: &[String]) -> bool {
    let program = base(&argv[0]).to_ascii_lowercase();
    if program == "xdg-terminal-exec" || program == "kitten" {
        return argv.len() > 1;
    }
    argv[1..].iter().any(|a| {
        let l = a.to_ascii_lowercase();
        !a.starts_with('-')
            || a == "--"
            || a == "-e"
            || a == "-x"
            || l.starts_with("--command")
            || l.starts_with("--exec")
            || l.starts_with("-e=")
            || (a.starts_with('-') && !a.starts_with("--") && a.len() > 2 && a[1..].contains('e'))
            || ["shell", "prog", "command", "exec", "session", "config", "hold", "startup"].iter().any(|w| l.contains(w))
    })
}

/// `git` options and subcommands that run a program or a configured command.
fn git_runs_command(argv: &[String]) -> bool {
    argv[1..].iter().enumerate().any(|(i, a)| {
        let l = a.to_ascii_lowercase();
        a == "-c"
            || (a.starts_with("-c") && !a.starts_with("--") && a.len() > 2)
            || l.starts_with("--config-env")
            || l.starts_with("--exec-path=")
            || l.starts_with("--upload-pack")
            || l.starts_with("--receive-pack")
            || l.starts_with("--exec")
            || l.starts_with("--extcmd")
            || a == "-x"
            || l.starts_with("ext::")
            || ["foreach", "filter-branch", "filter-repo"].contains(&l.as_str())
            || (l == "run" && i > 0 && argv[i] == "bisect")
            || (l == "config"
                && argv[i + 2..].iter().any(|v| {
                    let v = v.to_ascii_lowercase();
                    v.contains('!') || ["alias", "core.", "command", "helper", "program", "editor", "pager", "hook", "filter", "difftool", "mergetool", "url."].iter().any(|w| v.contains(w))
                }))
    })
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
    [
        "autostart",
        ".config/systemd",
        "environment.d",
        ".config/fish",
        "/etc/profile",
        "/etc/xdg",
        ".config/plasma-workspace",
        "cron",
        // Hyprland's config runs `exec`/`exec-once` lines; .desktop files and the defaults list
        // decide what an app name or a file opens; KWin scripts run inside the compositor;
        // COSMIC's custom shortcuts spawn commands.
        ".config/hypr",
        ".local/share/applications",
        ".local/share/kwin/scripts",
        "com.system76.cosmicsettings.shortcuts",
    ].iter().any(|s| a.contains(s))
        || [".bashrc", ".bash_profile", ".bash_login", ".bash_logout", ".profile", ".zshrc", ".zshenv", ".zprofile", ".zlogin", ".xprofile", ".xinitrc", ".xsession", ".pam_environment", "config.fish", "hyprland.conf", "mimeapps.list"].contains(&file)
}

/// Why this argv must never run (None: it may, after Enter).
pub fn blocked_argv(argv: &[String]) -> Option<&'static str> {
    if argv.is_empty() {
        return Some(WHY_SHELL);
    }
    if is_wrapper(&argv[0]) {
        // `env`, `nice`, `timeout`, `flock` … : judge every suffix argv[k..], since an option's
        // value (`env -u HOME rm`, `timeout -s KILL 10 rm`) must not be taken for the program.
        for k in 0..argv.len() {
            let rest = &argv[k..];
            let why = if is_wrapper(&rest[0]) {
                if wrapper_runs_text(rest) {
                    Some(WHY_SHELL)
                } else {
                    rest[1..].iter().find_map(|a| if secret_path(a) { Some(WHY_SECRETS) } else if startup_path(a) { Some(WHY_AUTOSTART) } else { None })
                }
            } else {
                blocked_plain(rest)
            };
            if why.is_some() {
                return why;
            }
        }
        return None;
    }
    blocked_plain(argv)
}

/// `blocked_argv` for an argv whose program is not a wrapper.
fn blocked_plain(argv: &[String]) -> Option<&'static str> {
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
    if TERMINALS.contains(&p) && terminal_runs_command(argv) {
        return Some(WHY_SHELL);
    }
    if p == "git" && git_runs_command(argv) {
        return Some(WHY_SHELL);
    }
    if ["make", "gmake", "bmake", "just"].contains(&p) {
        // runs whatever recipes the Makefile/justfile holds
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

    /// argv given as a list (elements may hold spaces).
    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn wrappers_fail_closed() {
        // an option value must not be taken for the program
        for (cmd, why) in [
            ("env -u HOME rm x", WHY_DELETE),
            ("env -C /tmp rm x", WHY_DELETE),
            ("flock /tmp/l rm -rf /tmp/x", WHY_DELETE),
            ("timeout -s KILL 10 rm x", WHY_DELETE),
            ("timeout --signal KILL 10 sudo x", WHY_ADMIN),
            ("taskset 0x1 rm x", WHY_DELETE),
            ("taskset -c 0-3 pacman -Syu", WHY_PACKAGES),
            ("systemd-run --unit x rm x", WHY_DELETE),
            ("systemd-run --user --unit=x --property Nice=5 rm x", WHY_DELETE),
            ("chrt 10 rm x", WHY_DELETE),
            ("chrt -r 10 rm x", WHY_DELETE),
            ("ionice -c 3 rm x", WHY_DELETE),
            ("nice -n 5 env -u X nohup timeout 5 rm x", WHY_DELETE),
            ("stdbuf -o L rm x", WHY_DELETE),
            ("uwsm app -- rm x", WHY_DELETE),
            ("env -u HOME cat /home/u/.ssh/id_rsa", WHY_SECRETS),
            ("flock /home/u/.bashrc true", WHY_AUTOSTART),
            ("systemd-run -p ExecStartPre=/bin/true true", WHY_SHELL),
            ("env -iS x", WHY_SHELL),
        ] {
            assert_eq!(blocked_argv(&argv(cmd)), Some(why), "{cmd}");
        }
        // `env -S` splits a string into a command line: blocked outright
        for cmd in [&["env", "-S", "rm /tmp/victim"][..], &["env", "--split-string=rm x"], &["env", "--split-string", "ls"], &["/usr/bin/env", "-vS", "true"], &["nice", "env", "-S", "true"]] {
            assert_eq!(blocked_argv(&list(cmd)), Some(WHY_SHELL), "{cmd:?}");
        }
        for ok in ["nice -n 5 cargo build", "timeout 10 uname -r", "env FOO=1 uname", "env -u HOME ls", "taskset 0x1 df -h", "setsid -f firefox"] {
            assert_eq!(blocked_argv(&argv(ok)), None, "{ok}");
        }
        // a long chain of wrappers stays cheap (no exponential recursion)
        let mut chain = vec!["env"; 30];
        chain.push("rm");
        assert_eq!(blocked_argv(&list(&chain)), Some(WHY_DELETE));
    }

    #[test]
    fn command_runners() {
        for cmd in [
            "kitty rm x",
            "kitty -e rm x",
            "kitty --hold sh",
            "kitty -o shell=rm",
            "kitten @ launch rm",
            "alacritty -e rm x",
            "alacritty --command rm x",
            "konsole -e rm",
            "foot rm x",
            "footclient rm x",
            "wezterm start -- rm x",
            "ghostty -e rm",
            "gnome-terminal -- rm x",
            "cosmic-term -e rm",
            "xterm -e rm",
            "xterm -hold -e rm",
            "/usr/bin/xdg-terminal-exec rm x",
            "git -c alias.x=!rm x",
            "git -c core.sshCommand=rm fetch",
            "git -c core.pager=rm log",
            "git -ccore.pager=rm log",
            "git --config-env=core.pager=X log",
            "git config alias.x !rm",
            "git config --global core.pager rm",
            "git rebase --exec rm HEAD~1",
            "git rebase -x rm HEAD~1",
            "git submodule foreach rm",
            "git bisect run rm",
            "git clone ext::rm",
            "git clone --upload-pack=rm x",
            "make",
            "make install",
            "/usr/bin/gmake -C /tmp",
            "just",
            "env make",
        ] {
            assert_eq!(blocked_argv(&argv(cmd)), Some(WHY_SHELL), "{cmd}");
        }
        for ok in ["kitty", "alacritty", "foot", "konsole", "git log --oneline", "git -C /tmp status", "git config --get user.name", "git diff --cached"] {
            assert_eq!(blocked_argv(&argv(ok)), None, "{ok}");
        }
    }

    #[test]
    fn startup_paths() {
        for cmd in [
            "cp x /home/u/.config/hypr/hyprland.conf",
            "tee -a /home/u/.config/hypr/autoexec.conf",
            "ln -s x /home/u/.config/hypr",
            "cp x hyprland.conf",
            "cp x /home/u/.local/share/applications/firefox.desktop",
            "tee /home/u/.config/mimeapps.list",
            "cp x /home/u/.config/plasma-workspace/env/x.sh",
            "cp x /home/u/.config/plasma-workspace/shutdown/x.sh",
            "cp -r x /home/u/.local/share/kwin/scripts/x",
            "cp x /home/u/.config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom",
        ] {
            assert_eq!(blocked_argv(&argv(cmd)), Some(WHY_AUTOSTART), "{cmd}");
        }
        assert_eq!(blocked_argv(&argv("ls /home/u/.config/cosmic")), None);
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
