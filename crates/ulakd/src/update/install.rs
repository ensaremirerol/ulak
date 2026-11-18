//! Atomic replacement of the running binary, the previous copy, and the rollback marker.
//! Names follow the executable: `ulakd` → `ulakd.new`/`ulakd.prev`, `ulak.exe` → `ulak.new`/`ulak.prev`.
//!
//! Flow: `install` writes `<stem>.new`, copies the current binary to `<stem>.prev`, writes the
//! marker (`update-pending.json` for ulakd, `<stem>-update-pending.json` otherwise, in the config
//! dir) and renames the new file over the executable. Windows cannot rename over a running
//! `.exe`, so there the running file is first moved aside to `<stem>.old`.
//! First start afterwards (`startup`): marker gets `started_ms` (probation). If the process lives
//! 30 s, `confirm` deletes the marker. If it dies sooner, the next start sees a marker with
//! `started_ms` set, restores `ulakd.prev`, and records the bad version in `update-skip` so the
//! auto path does not fetch it again.
use crate::errors::UpdateError;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

/// Mode 0755 on Unix; self-update is Unix-only in practice (the daemon is Linux), a no-op elsewhere.
#[cfg(unix)]
fn make_exec(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))
}
#[cfg(not(unix))]
fn make_exec(_: &Path) -> std::io::Result<()> {
    Ok(())
}

trait ModeExec {
    fn mode_exec(&mut self) -> &mut Self;
}
impl ModeExec for std::fs::OpenOptions {
    fn mode_exec(&mut self) -> &mut Self {
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(self, 0o755);
        self
    }
}

pub const CONFIRM_AFTER_SECS: u64 = 30;
/// A marker whose first run is older than this is stale (reboot, long outage): do not roll back.
const STALE_MS: u64 = 300_000;

fn stem(exe: &Path) -> String {
    exe.file_stem()
        .map_or_else(|| "ulakd".into(), |s| s.to_string_lossy().into_owned())
}

fn sibling(exe: &Path, ext: &str) -> PathBuf {
    exe.with_file_name(format!("{}.{ext}", stem(exe)))
}

/// ulakd keeps the original names so a marker written by an older ulakd is still honoured.
fn marker(exe: &Path) -> String {
    match stem(exe).as_str() {
        "ulakd" => "update-pending.json".into(),
        s => format!("{s}-update-pending.json"),
    }
}

fn skip_file(exe: &Path) -> String {
    match stem(exe).as_str() {
        "ulakd" => "update-skip".into(),
        s => format!("{s}-update-skip"),
    }
}

/// Puts `src` at `exe`. Unix: one atomic rename. Windows: the running image can be renamed but not
/// replaced, so it moves to `<stem>.old` first (deleted on the next install).
fn swap_in(src: &Path, exe: &Path) -> std::io::Result<()> {
    if cfg!(windows) {
        let old = sibling(exe, "old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old)?;
        if let Err(e) = std::fs::rename(src, exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(e);
        }
        Ok(())
    } else {
        std::fs::rename(src, exe)
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Marker {
    pub from: String,
    pub to: String,
    pub started_ms: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Startup {
    Normal,
    /// First run of a freshly installed binary; call `confirm` once it has been up 30 s.
    Probation,
    /// The new binary died early; `ulakd.prev` is back in place. Restart to run it.
    RolledBack {
        bad: String,
    },
}

pub fn prev_path(exe: &Path) -> PathBuf {
    sibling(exe, "prev")
}

/// Replaces `exe` with `new_bin`. On any error before the final rename the old binary is untouched.
pub fn install(
    exe: &Path,
    new_bin: &[u8],
    conf_dir: &Path,
    from: &str,
    to: &str,
) -> Result<(), UpdateError> {
    let dir = exe
        .parent()
        .ok_or_else(|| UpdateError::NotWritable(exe.display().to_string()))?;
    let tmp = sibling(exe, "new");
    let _ = std::fs::remove_file(&tmp);
    let not_writable = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem => {
            UpdateError::NotWritable(dir.display().to_string())
        }
        _ => UpdateError::Io(e),
    };
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode_exec()
        .open(&tmp)
        .map_err(not_writable)?;
    let res = (|| -> Result<(), UpdateError> {
        f.write_all(new_bin)?;
        f.sync_all()?;
        make_exec(&tmp)?;
        let prev = prev_path(exe);
        let _ = std::fs::remove_file(&prev);
        std::fs::copy(exe, &prev)?;
        std::fs::create_dir_all(conf_dir)?;
        let m = Marker {
            from: from.into(),
            to: to.into(),
            started_ms: None,
        };
        std::fs::write(conf_dir.join(marker(exe)), serde_json::to_string(&m)?)?;
        swap_in(&tmp, exe)?;
        Ok(())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(conf_dir.join(marker(exe)));
    }
    res
}

/// Call once at `ulakd run` start.
pub fn startup(conf_dir: &Path, exe: &Path, now_ms: u64) -> Result<Startup, UpdateError> {
    let path = conf_dir.join(marker(exe));
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Startup::Normal),
        Err(e) => return Err(e.into()),
    };
    let Ok(mut m) = serde_json::from_str::<Marker>(&text) else {
        let _ = std::fs::remove_file(&path); // unreadable marker: do not act on it
        return Ok(Startup::Normal);
    };
    match m.started_ms {
        None => {
            m.started_ms = Some(now_ms);
            std::fs::write(&path, serde_json::to_string(&m)?)?;
            Ok(Startup::Probation)
        }
        Some(t) if now_ms.saturating_sub(t) > STALE_MS => {
            std::fs::remove_file(&path)?;
            Ok(Startup::Normal)
        }
        Some(_) => {
            let prev = prev_path(exe);
            if prev.exists() {
                let tmp = sibling(exe, "restore");
                std::fs::copy(&prev, &tmp)?;
                make_exec(&tmp)?;
                swap_in(&tmp, exe)?;
            }
            std::fs::write(conf_dir.join(skip_file(exe)), &m.to)?;
            std::fs::remove_file(&path)?;
            Ok(Startup::RolledBack { bad: m.to })
        }
    }
}

/// The new binary has proven itself.
pub fn confirm(conf_dir: &Path, exe: &Path) {
    let _ = std::fs::remove_file(conf_dir.join(marker(exe)));
}

/// A version that was rolled back; the auto path must not fetch it again.
pub fn skipped(conf_dir: &Path, exe: &Path) -> Option<String> {
    std::fs::read_to_string(conf_dir.join(skip_file(exe)))
        .ok()
        .map(|s| s.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("bin");
        let conf = d.path().join("conf");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("ulakd");
        std::fs::write(&exe, "OLD").unwrap();
        (d, exe, conf)
    }

    #[test]
    fn install_swaps_atomically_and_keeps_prev() {
        let (_d, exe, conf) = setup();
        install(&exe, b"NEW", &conf, "0.1.0", "0.2.0").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"NEW");
        assert_eq!(std::fs::read(prev_path(&exe)).unwrap(), b"OLD");
        assert_eq!(
            std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!exe.with_file_name("ulakd.new").exists());
        assert!(conf.join(marker(&exe)).exists());
    }

    #[test]
    fn unwritable_dir_fails_clearly_and_changes_nothing() {
        let (_d, exe, conf) = setup();
        let dir = exe.parent().unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let r = install(&exe, b"NEW", &conf, "0.1.0", "0.2.0");
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        if unsafe_is_root() {
            return; // root ignores directory modes
        }
        assert!(matches!(r, Err(UpdateError::NotWritable(_))), "{r:?}");
        assert_eq!(std::fs::read(&exe).unwrap(), b"OLD");
        assert!(!conf.join(marker(&exe)).exists());
    }

    fn unsafe_is_root() -> bool {
        std::fs::read_to_string("/proc/self/status").is_ok_and(|s| {
            s.lines()
                .any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(1) == Some("0"))
        })
    }

    #[test]
    fn no_marker_is_normal() {
        let (_d, exe, conf) = setup();
        assert_eq!(startup(&conf, &exe, 1).unwrap(), Startup::Normal);
    }

    #[test]
    fn confirmed_update_is_kept() {
        let (_d, exe, conf) = setup();
        install(&exe, b"NEW", &conf, "0.1.0", "0.2.0").unwrap();
        assert_eq!(startup(&conf, &exe, 1_000).unwrap(), Startup::Probation);
        confirm(&conf, &exe); // lived 30 s
        assert_eq!(startup(&conf, &exe, 40_000).unwrap(), Startup::Normal);
        assert_eq!(std::fs::read(&exe).unwrap(), b"NEW");
        assert_eq!(skipped(&conf, &exe), None);
    }

    #[test]
    fn early_death_rolls_back_once_and_remembers_the_bad_version() {
        let (_d, exe, conf) = setup();
        install(&exe, b"NEW", &conf, "0.1.0", "0.2.0").unwrap();
        assert_eq!(startup(&conf, &exe, 1_000).unwrap(), Startup::Probation);
        // dies before confirm; supervisor restarts it 3 s later
        assert_eq!(
            startup(&conf, &exe, 4_000).unwrap(),
            Startup::RolledBack {
                bad: "0.2.0".into()
            }
        );
        assert_eq!(std::fs::read(&exe).unwrap(), b"OLD");
        assert_eq!(
            std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(skipped(&conf, &exe).as_deref(), Some("0.2.0"));
        assert_eq!(startup(&conf, &exe, 5_000).unwrap(), Startup::Normal);
    }

    #[test]
    fn stale_marker_does_not_roll_back() {
        let (_d, exe, conf) = setup();
        install(&exe, b"NEW", &conf, "0.1.0", "0.2.0").unwrap();
        startup(&conf, &exe, 1_000).unwrap();
        assert_eq!(
            startup(&conf, &exe, 1_000 + STALE_MS + 1).unwrap(),
            Startup::Normal
        );
        assert_eq!(std::fs::read(&exe).unwrap(), b"NEW");
    }

    #[test]
    fn app_names_do_not_collide_with_ulakd() {
        let (d, exe, conf) = setup();
        let app = exe.with_file_name("ulak.exe");
        std::fs::write(&app, "OLDAPP").unwrap();
        install(&app, b"NEWAPP", &conf, "0.1.0", "0.2.0").unwrap();
        assert_eq!(std::fs::read(&app).unwrap(), b"NEWAPP");
        assert_eq!(std::fs::read(prev_path(&app)).unwrap(), b"OLDAPP");
        assert!(prev_path(&app).ends_with("ulak.prev"));
        assert!(conf.join("ulak-update-pending.json").exists());
        assert_eq!(
            startup(&conf, &exe, 1).unwrap(),
            Startup::Normal,
            "ulakd unaffected"
        );
        assert_eq!(startup(&conf, &app, 1).unwrap(), Startup::Probation);
        assert_eq!(std::fs::read(&exe).unwrap(), b"OLD");
        drop(d);
    }

    #[test]
    fn garbage_marker_is_ignored() {
        let (_d, exe, conf) = setup();
        std::fs::create_dir_all(&conf).unwrap();
        std::fs::write(conf.join(marker(&exe)), "{{").unwrap();
        assert_eq!(startup(&conf, &exe, 1).unwrap(), Startup::Normal);
        assert!(!conf.join(marker(&exe)).exists());
    }
}
