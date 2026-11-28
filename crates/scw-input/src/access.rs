//! Letting sc-watch read keyboard keys on Linux: only the `input` group may
//! read a keyboard there (reading one is how a keylogger works), so the page
//! offers to add this user to it - through the desktop's own password
//! dialog (polkit's pkexec), never by asking for the password itself. The
//! change takes effect at the next login.
//!
//! Nothing here touches the game: sc-watch only asks each device whether a
//! key is down (as GetAsyncKeyState on Windows), opens every device
//! read-only, never grabs one and never sends anything to one.
//!
//! The files are read as text, so the reading is tested on any system; only
//! running the setup is Linux's.

/// Where keyboard keys stand for this user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAccess {
    /// Keyboards are read (or there is nothing closed to read).
    Readable,
    /// This user is in the `input` group, but this login began before:
    /// logging out of the desktop and back in finishes it.
    AfterLogin,
    /// Keyboards are closed to this user: the group is needed.
    Needed,
}

/// Run as root by pkexec, with the user's name as `$1`: the `input` group
/// copied into /etc/group when only /usr/lib/group has it (Fedora Atomic -
/// Bazzite - keeps system groups there, where usermod cannot add to them),
/// then the user added to it.
pub const SETUP_SCRIPT: &str = r#"set -e
user="$1"
if ! grep -q '^input:' /etc/group; then
  if [ -f /usr/lib/group ] && grep -q '^input:' /usr/lib/group; then
    grep '^input:' /usr/lib/group >> /etc/group
  else
    echo "this system has no 'input' group" >&2
    exit 3
  fi
fi
usermod -aG input "$user"
"#;

/// The command that runs [`SETUP_SCRIPT`] for `user` through the desktop's
/// password dialog. The name is an argument, never part of the script.
pub fn setup_command(user: &str) -> Vec<String> {
    [
        "pkexec",
        "/bin/sh",
        "-c",
        SETUP_SCRIPT,
        "sc-watch-keyboard-setup",
        user,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// What pkexec's exit code (or the script's) means, as the page says it.
pub fn setup_outcome(code: Option<i32>, stderr: &str) -> Result<(), String> {
    match code {
        Some(0) => Ok(()),
        Some(126) => Err("the password dialog was closed or refused - nothing changed".into()),
        Some(127) => Err(
            "not allowed - no password dialog could be shown, or the password was not \
             accepted; nothing changed"
                .into(),
        ),
        Some(3) => Err("this system has no 'input' group - nothing changed".into()),
        other => {
            let why = stderr.trim();
            Err(match other {
                Some(c) => format!("the setup failed (exit code {c}){}", tail(why)),
                None => format!("the setup was stopped{}", tail(why)),
            })
        }
    }
}

fn tail(why: &str) -> String {
    if why.is_empty() {
        String::new()
    } else {
        format!(": {}", why.lines().last().unwrap_or(why))
    }
}

/// The real user id in /proc/self/status's text.
pub fn uid_in_status(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// The groups this process has (its login's), in /proc/self/status's text.
pub fn gids_in_status(status: &str) -> Vec<u32> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("Groups:"))
        .map(|g| {
            g.split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// The name of user `uid` in a passwd file's text.
pub fn user_named(uid: u32, passwd: &str) -> Option<String> {
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 3 && f[2].parse::<u32>().ok() == Some(uid) && !f[0].is_empty())
            .then(|| f[0].to_string())
    })
}

/// Group `name` in a group file's text: its id and its listed members.
pub fn group_entry(name: &str, group: &str) -> Option<(u32, Vec<String>)> {
    group.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() < 3 || f[0] != name {
            return None;
        }
        let members = f
            .get(3)
            .map(|m| {
                m.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        Some((f[2].parse().ok()?, members))
    })
}

/// Where keys stand, from what was found: keyboards readable now, and else
/// whether `user` is listed in the `input` group (`/etc/group`'s text) and
/// whether this login has that group (`session` gids).
pub fn key_access(
    keyboards_readable: bool,
    user: Option<&str>,
    etc_group: &str,
    session: &[u32],
) -> KeyAccess {
    if keyboards_readable {
        return KeyAccess::Readable;
    }
    match (user, group_entry("input", etc_group)) {
        (Some(u), Some((gid, members)))
            if members.iter().any(|m| m == u) && !session.contains(&gid) =>
        {
            KeyAccess::AfterLogin
        }
        _ => KeyAccess::Needed,
    }
}

/// This process's user name: its real uid looked up in /etc/passwd, then in
/// /usr/lib/passwd (Fedora Atomic's system users).
#[cfg(target_os = "linux")]
pub fn current_user() -> Option<String> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let uid = uid_in_status(&status)?;
    ["/etc/passwd", "/usr/lib/passwd"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .find_map(|t| user_named(uid, &t))
}

/// Runs the setup for this user through the desktop's password dialog, and
/// waits for it (the dialog waits for the user).
#[cfg(target_os = "linux")]
pub fn grant() -> Result<(), String> {
    let user = current_user().ok_or("which user this is could not be told - nothing changed")?;
    let cmd = setup_command(&user);
    let out = std::process::Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| {
            format!("pkexec could not run ({e}) - is polkit installed? Nothing changed")
        })?;
    setup_outcome(out.status.code(), &String::from_utf8_lossy(&out.stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "Name:\tsc-watch\nUmask:\t0022\nUid:\t1000\t1000\t1000\t1000\n\
                          Gid:\t1000\t1000\t1000\t1000\nGroups:\t10 1000 \n";

    #[test]
    fn who_and_which_groups_are_read_from_the_system_files() {
        assert_eq!(uid_in_status(STATUS), Some(1000));
        assert_eq!(gids_in_status(STATUS), vec![10, 1000]);
        assert_eq!(gids_in_status("Groups:\n"), Vec::<u32>::new());
        let passwd = "root:x:0:0:root:/root:/bin/bash\nexmpl:x:1000:1000::/home/exmpl:/bin/bash\n";
        assert_eq!(user_named(1000, passwd), Some("exmpl".into()));
        assert_eq!(user_named(1001, passwd), None);
        let group = "wheel:x:10:exmpl\ninput:x:104:other, exmpl\nvideo:x:39:\n";
        assert_eq!(
            group_entry("input", group),
            Some((104, vec!["other".into(), "exmpl".into()]))
        );
        assert_eq!(group_entry("video", group), Some((39, vec![])));
        assert_eq!(group_entry("audio", group), None);
    }

    #[test]
    fn access_is_readable_after_login_or_needed() {
        let listed = "input:x:104:exmpl\n";
        let user = Some("exmpl");
        assert_eq!(key_access(true, user, "", &[]), KeyAccess::Readable);
        // listed, but this login has not got the group yet
        assert_eq!(
            key_access(false, user, listed, &[1000]),
            KeyAccess::AfterLogin
        );
        // listed and in this login, still unreadable: set it up again
        assert_eq!(key_access(false, user, listed, &[104]), KeyAccess::Needed);
        // not listed, or no group in /etc/group at all (Bazzite before setup)
        assert_eq!(
            key_access(false, user, "input:x:104:\n", &[]),
            KeyAccess::Needed
        );
        assert_eq!(key_access(false, user, "", &[]), KeyAccess::Needed);
        assert_eq!(key_access(false, None, listed, &[]), KeyAccess::Needed);
    }

    #[test]
    fn the_setup_names_the_user_only_as_an_argument() {
        let cmd = setup_command("exmpl; rm -rf /");
        assert_eq!(&cmd[..3], ["pkexec", "/bin/sh", "-c"]);
        assert_eq!(cmd[3], SETUP_SCRIPT);
        assert_eq!(cmd[5], "exmpl; rm -rf /", "passed as $1, never spliced in");
        assert!(!SETUP_SCRIPT.contains("exmpl"));
        assert!(SETUP_SCRIPT.contains(r#"usermod -aG input "$user""#));
        assert!(SETUP_SCRIPT.starts_with("set -e\n"));
    }

    #[test]
    fn the_outcome_is_said_plainly() {
        assert_eq!(setup_outcome(Some(0), ""), Ok(()));
        assert!(
            setup_outcome(Some(126), "")
                .unwrap_err()
                .contains("closed or refused")
        );
        assert!(
            setup_outcome(Some(127), "")
                .unwrap_err()
                .contains("not allowed")
        );
        assert!(
            setup_outcome(Some(3), "")
                .unwrap_err()
                .contains("no 'input' group")
        );
        assert_eq!(
            setup_outcome(Some(6), "usermod: something\nusermod: the last line\n"),
            Err("the setup failed (exit code 6): usermod: the last line".into())
        );
        assert_eq!(setup_outcome(None, ""), Err("the setup was stopped".into()));
    }
}
