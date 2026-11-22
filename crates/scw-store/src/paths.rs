//! Where things live (paths.py): the program, and the data it writes about
//! you - which are not one answer once the tool is installed.
//!
//! The program is its executable's folder. The data is `data\` beside it
//! when a `portable.txt` sits there (the standalone zip), else
//! %LOCALAPPDATA%\sc-watch. `SCW_DATA_ROOT` names another folder outright:
//! for development and tests, which must never write into somebody's real
//! data.

use std::path::{Path, PathBuf};

/// How the data folder was chosen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Installed,
    Portable,
    /// SCW_DATA_ROOT.
    Chosen,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Installed => "installed",
            Mode::Portable => "portable",
            Mode::Chosen => "SCW_DATA_ROOT",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Roots {
    pub mode: Mode,
    /// The program's folder.
    pub program: PathBuf,
    /// Everything written about you.
    pub data: PathBuf,
}

impl Roots {
    /// The roots for the program at `exe`, with `env` for the environment.
    pub fn resolve(exe: &Path, env: &dyn Fn(&str) -> Option<String>) -> Roots {
        let program = exe
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        if let Some(d) = env("SCW_DATA_ROOT").filter(|d| !d.trim().is_empty()) {
            return Roots {
                mode: Mode::Chosen,
                program,
                data: PathBuf::from(d),
            };
        }
        if program.join("portable.txt").exists() {
            return Roots {
                mode: Mode::Portable,
                data: program.join("data"),
                program,
            };
        }
        // LOCALAPPDATA, not APPDATA: a local database and cache, which
        // should not follow a roaming profile between machines.
        let base = env("LOCALAPPDATA")
            .filter(|b| !b.is_empty())
            .or_else(|| env("HOME"))
            .or_else(|| env("USERPROFILE"))
            .unwrap_or_else(|| ".".into());
        Roots {
            mode: Mode::Installed,
            program,
            data: PathBuf::from(base).join("sc-watch"),
        }
    }

    /// The roots for this running program.
    pub fn current() -> Roots {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sc-watch"));
        Roots::resolve(&exe, &|k| std::env::var(k).ok())
    }

    /// A file the tool writes; its folder is made on demand.
    pub fn data(&self, parts: &[&str]) -> PathBuf {
        let p = parts.iter().fold(self.data.clone(), |p, s| p.join(s));
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        p
    }

    /// A folder the tool writes into, made if absent.
    pub fn data_dir(&self, parts: &[&str]) -> PathBuf {
        let p = parts.iter().fold(self.data.clone(), |p, s| p.join(s));
        let _ = std::fs::create_dir_all(&p);
        p
    }

    /// An older install's data this copy is not using (paths.unused_old_data):
    /// every 0.1.x build kept it in %LOCALAPPDATA%\sc-watch, and the portable
    /// zip keeps it beside the program, so after an upgrade it can look
    /// lost. Only ever reported. Quiet once the data here holds at least as
    /// many players as the old database.
    pub fn unused_old_data(&self, env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
        if self.mode != Mode::Portable {
            return None;
        }
        let old = PathBuf::from(env("LOCALAPPDATA").filter(|b| !b.is_empty())?).join("sc-watch");
        let same = match (old.canonicalize(), self.data.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        };
        if same || !old.join("sc-watch.db").is_file() {
            return None;
        }
        let players = |db: &Path| -> Option<i64> {
            if !db.is_file() {
                return Some(0);
            }
            let c = rusqlite::Connection::open_with_flags(
                db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .ok()?;
            c.query_row("SELECT COUNT(*) FROM players", [], |r| r.get(0))
                .ok()
        };
        match (
            players(&self.data.join("sc-watch.db")),
            players(&old.join("sc-watch.db")),
        ) {
            (Some(here), Some(there)) if here >= there => None,
            // cannot tell: say so rather than stay quiet
            _ => Some(old),
        }
    }

    /// Where everything is (paths.describe), for --where.
    pub fn describe(&self, env: &dyn Fn(&str) -> Option<String>) -> String {
        let mut out = format!(
            "  mode      {}\n  program   {}\n  your data {}",
            self.mode.as_str(),
            self.program.display(),
            self.data.display()
        );
        if let Some(old) = self.unused_old_data(env) {
            out.push_str(&format!(
                "\n  NOT USED  {} (an older install's data - see Upgrading in the README)",
                old.display()
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("scw-paths-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn data_folder_by_mode() {
        let prog = temp("prog");
        let local = temp("local");
        let l = local.display().to_string();
        let env = |k: &str| (k == "LOCALAPPDATA").then(|| l.clone());
        let r = Roots::resolve(&prog.join("sc-watch.exe"), &env);
        assert_eq!(r.mode, Mode::Installed);
        assert_eq!(r.data, local.join("sc-watch"));

        std::fs::write(prog.join("portable.txt"), "").unwrap();
        let r = Roots::resolve(&prog.join("sc-watch.exe"), &env);
        assert_eq!(
            (r.mode, r.data.clone()),
            (Mode::Portable, prog.join("data"))
        );
        // nothing old to report until the old folder has a database
        assert_eq!(r.unused_old_data(&env), None);
        let old = local.join("sc-watch");
        std::fs::create_dir_all(&old).unwrap();
        let c = rusqlite::Connection::open(old.join("sc-watch.db")).unwrap();
        c.execute_batch("CREATE TABLE players (handle TEXT); INSERT INTO players VALUES ('A');")
            .unwrap();
        drop(c);
        assert_eq!(r.unused_old_data(&env), Some(old.clone()));
        assert!(r.describe(&env).contains("NOT USED"));
        // a copy here with as many players quiets it
        std::fs::create_dir_all(prog.join("data")).unwrap();
        std::fs::copy(
            old.join("sc-watch.db"),
            prog.join("data").join("sc-watch.db"),
        )
        .unwrap();
        assert_eq!(r.unused_old_data(&env), None);

        let chosen = |k: &str| (k == "SCW_DATA_ROOT").then(|| "X:/elsewhere".to_string());
        let r = Roots::resolve(&prog.join("sc-watch.exe"), &chosen);
        assert_eq!(
            (r.mode, r.data),
            (Mode::Chosen, PathBuf::from("X:/elsewhere"))
        );
        let _ = std::fs::remove_dir_all(&prog);
        let _ = std::fs::remove_dir_all(&local);
    }
}
