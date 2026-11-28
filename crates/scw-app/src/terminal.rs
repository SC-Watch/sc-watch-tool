//! Started without a terminal - double-clicked in the file manager, as
//! sc-watch.exe is on Windows - the Linux build opens one for itself and
//! runs there: the same window to watch it in and close to stop it that
//! Windows gives a console program. Started from a terminal, or with any
//! option, nothing changes.
//!
//! The choice of terminal is a pure function of the desktop and what is
//! installed, so it is tested on any system; only the relaunch itself is
//! Linux's.

/// Set in the terminal's copy, so it never relaunches again; also says the
/// window is its own, to keep open after an error until it is read.
pub const IN_TERMINAL: &str = "SCW_IN_TERMINAL";

/// Whether this start should move into a terminal: no options (a file
/// manager passes none), no terminal on either end, a desktop to open one
/// on, and not already the terminal's copy.
pub fn wanted(
    args: &[String],
    stdin_tty: bool,
    stdout_tty: bool,
    env: &dyn Fn(&str) -> Option<String>,
) -> bool {
    let set = |k: &str| env(k).is_some_and(|v| !v.trim().is_empty());
    args.is_empty()
        && !stdin_tty
        && !stdout_tty
        && !set(IN_TERMINAL)
        && (set("WAYLAND_DISPLAY") || set("DISPLAY"))
}

/// The command that runs `exe` in a terminal window: the desktop's own
/// choice when it says (xdg-terminal-exec), else KDE's Konsole on KDE, else
/// the first of the usual ones that is installed. None when there is none.
pub fn command_for(
    exe: &str,
    desktop: &str,
    installed: &dyn Fn(&str) -> bool,
) -> Option<(String, Vec<String>)> {
    let kde = desktop.split(':').any(|d| d.eq_ignore_ascii_case("kde"));
    // Ptyxis reads its command as one shell-quoted line.
    let quoted = format!("'{}'", exe.replace('\'', r"'\''"));
    let with = |prefix: &[&str], last: &str| -> Vec<String> {
        prefix
            .iter()
            .map(|s| s.to_string())
            .chain(std::iter::once(last.to_string()))
            .collect()
    };
    let mut order = vec![
        ("xdg-terminal-exec", with(&[], exe)),
        ("ptyxis", with(&["--new-window", "-x"], &quoted)),
        ("gnome-terminal", with(&["--"], exe)),
        ("kgx", with(&["--"], exe)),
        ("konsole", with(&["-e"], exe)),
        ("xfce4-terminal", with(&["-x"], exe)),
        ("xterm", with(&["-e"], exe)),
    ];
    if kde {
        let konsole = order.remove(4);
        order.insert(1, konsole);
    }
    order
        .into_iter()
        .find(|(name, _)| installed(name))
        .map(|(name, args)| (name.to_string(), args))
}

/// Is `name` a program in one of `path`'s folders (as PATH lists them)?
pub fn on_path(name: &str, path: &std::ffi::OsStr) -> bool {
    std::env::split_paths(path)
        .filter(|d| !d.as_os_str().is_empty())
        .any(|d| d.join(name).is_file())
}

/// Moves this start into a terminal window when [`wanted`]: Some(exit code)
/// when the terminal took over, None to carry on here (wanted or not, and
/// when no terminal could be opened).
#[cfg(target_os = "linux")]
pub fn relaunch(args: &[String]) -> Option<i32> {
    use std::io::IsTerminal;
    let env = |k: &str| std::env::var(k).ok();
    if !wanted(
        args,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        &env,
    ) {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let (term, targs) = command_for(
        exe.to_str()?,
        &env("XDG_CURRENT_DESKTOP").unwrap_or_default(),
        &|n| on_path(n, &path),
    )?;
    let mut cmd = std::process::Command::new(&term);
    cmd.args(&targs).env(IN_TERMINAL, "1");
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    cmd.spawn().ok().map(|_| 0)
}

/// In the terminal sc-watch opened for itself, an exit with an error waits
/// for Enter: the window would otherwise close before the reason is read.
pub fn hold_on_error(rc: i32, env: &dyn Fn(&str) -> Option<String>) {
    if rc == 0 || rc == 130 || env(IN_TERMINAL).is_none_or(|v| v.trim().is_empty()) {
        return;
    }
    eprintln!("\nsc-watch stopped (exit code {rc}). Press Enter to close this window.");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn only_a_bare_start_without_a_terminal_moves() {
        let wayland = env_of(&[("WAYLAND_DISPLAY", "wayland-0")]);
        assert!(wanted(&[], false, false, &wayland));
        assert!(wanted(&[], false, false, &env_of(&[("DISPLAY", ":0")])));
        // any option: a script or a test runs it
        assert!(!wanted(&["--verbose".into()], false, false, &wayland));
        // a terminal on either end
        assert!(!wanted(&[], true, false, &wayland));
        assert!(!wanted(&[], false, true, &wayland));
        // no desktop to open a window on (a service, CI)
        assert!(!wanted(&[], false, false, &env_of(&[("DISPLAY", " ")])));
        assert!(!wanted(&[], false, false, &env_of(&[])));
        // already the terminal's copy
        assert!(!wanted(
            &[],
            false,
            false,
            &env_of(&[("WAYLAND_DISPLAY", "wayland-0"), (IN_TERMINAL, "1")])
        ));
    }

    #[test]
    fn the_terminal_is_the_desktops_own_first() {
        let exe = "/home/u/sc watch/sc-watch";
        let only = |names: &'static [&'static str]| move |n: &str| names.contains(&n);
        let pick = |desktop: &str, names: &'static [&'static str]| {
            command_for(exe, desktop, &only(names)).map(|(t, a)| (t, a.join("|")))
        };
        let all: &'static [&'static str] = &[
            "xdg-terminal-exec",
            "konsole",
            "ptyxis",
            "gnome-terminal",
            "kgx",
            "xfce4-terminal",
            "xterm",
        ];
        assert_eq!(
            pick("KDE", all),
            Some(("xdg-terminal-exec".into(), exe.into()))
        );
        // KDE: Konsole before the rest, whatever else is installed
        assert_eq!(
            pick("KDE", &all[1..]),
            Some(("konsole".into(), format!("-e|{exe}")))
        );
        assert_eq!(
            pick("ubuntu:GNOME", &all[1..]),
            Some((
                "ptyxis".into(),
                "--new-window|-x|'/home/u/sc watch/sc-watch'".into()
            ))
        );
        assert_eq!(
            pick("GNOME", &["gnome-terminal", "konsole", "xterm"]),
            Some(("gnome-terminal".into(), format!("--|{exe}")))
        );
        assert_eq!(
            pick("XFCE", &["xterm", "xfce4-terminal"]),
            Some(("xfce4-terminal".into(), format!("-x|{exe}")))
        );
        assert_eq!(
            pick("", &["xterm"]),
            Some(("xterm".into(), format!("-e|{exe}")))
        );
        assert_eq!(pick("KDE", &[]), None);
        // a quote in the path survives Ptyxis's shell-quoted line
        let (_, a) = command_for("/a/it's/sc-watch", "GNOME", &|n| n == "ptyxis").unwrap();
        assert_eq!(a[2], r"'/a/it'\''s/sc-watch'");
    }

    #[test]
    fn programs_are_found_on_the_path() {
        let dir = std::env::temp_dir().join(format!("scw-term-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("xterm"), b"").unwrap();
        let nowhere = dir.join("nowhere");
        let path = std::env::join_paths([nowhere.as_path(), dir.as_path()]).unwrap();
        assert!(on_path("xterm", &path));
        assert!(!on_path("konsole", &path));
        assert!(!on_path("xterm", std::ffi::OsStr::new("")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
