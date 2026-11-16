//! Atomic replacement of the running binary, the previous copy, and the rollback marker.
//!
//! Flow: `install` writes `ulakd.new`, copies the current binary to `ulakd.prev`, writes the
//! marker `update-pending.json` (config dir) and renames the new file over the executable.
//! First start afterwards (`startup`): marker gets `started_ms` (probation). If the process lives
//! 30 s, `confirm` deletes the marker. If it dies sooner, the next start sees a marker with
//! `started_ms` set, restores `ulakd.prev`, and records the bad version in `update-skip` so the
//! auto path does not fetch it again.
use crate::errors::UpdateError;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub const CONFIRM_AFTER_SECS: u64 = 30;
/// A marker whose first run is older than this is stale (reboot, long outage): do not roll back.
const STALE_MS: u64 = 300_000;
const MARKER: &str = "update-pending.json";
const SKIP: &str = "update-skip";

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
    exe.with_file_name("ulakd.prev")
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
    let tmp = dir.join("ulakd.new");
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
        .mode(0o755)
        .open(&tmp)
        .map_err(not_writable)?;
    let res = (|| -> Result<(), UpdateError> {
        f.write_all(new_bin)?;
        f.sync_all()?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        let prev = prev_path(exe);
        let _ = std::fs::remove_file(&prev);
        std::fs::copy(exe, &prev)?;
        std::fs::create_dir_all(conf_dir)?;
        let m = Marker {
            from: from.into(),
            to: to.into(),
            started_ms: None,
        };
        std::fs::write(conf_dir.join(MARKER), serde_json::to_string(&m)?)?;
        std::fs::rename(&tmp, exe)?;
        Ok(())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(conf_dir.join(MARKER));
    }
    res
}

/// Call once at `ulakd run` start.
pub fn startup(conf_dir: &Path, exe: &Path, now_ms: u64) -> Result<Startup, UpdateError> {
    let path = conf_dir.join(MARKER);
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
                let tmp = exe.with_file_name("ulakd.restore");
                std::fs::copy(&prev, &tmp)?;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
                std::fs::rename(&tmp, exe)?;
            }
            std::fs::write(conf_dir.join(SKIP), &m.to)?;
            std::fs::remove_file(&path)?;
            Ok(Startup::RolledBack { bad: m.to })
        }
    }
}

/// The new binary has proven itself.
pub fn confirm(conf_dir: &Path) {
    let _ = std::fs::remove_file(conf_dir.join(MARKER));
}

/// A version that was rolled back; the auto path must not fetch it again.
pub fn skipped(conf_dir: &Path) -> Option<String> {
    std::fs::read_to_string(conf_dir.join(SKIP))
        .ok()
        .map(|s| s.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(conf.join(MARKER).exists());
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
        assert!(!conf.join(MARKER).exists());
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
        confirm(&conf); // lived 30 s
        assert_eq!(startup(&conf, &exe, 40_000).unwrap(), Startup::Normal);
        assert_eq!(std::fs::read(&exe).unwrap(), b"NEW");
        assert_eq!(skipped(&conf), None);
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
        assert_eq!(skipped(&conf).as_deref(), Some("0.2.0"));
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
    fn garbage_marker_is_ignored() {
        let (_d, exe, conf) = setup();
        std::fs::create_dir_all(&conf).unwrap();
        std::fs::write(conf.join(MARKER), "{{").unwrap();
        assert_eq!(startup(&conf, &exe, 1).unwrap(), Startup::Normal);
        assert!(!conf.join(MARKER).exists());
    }
}
