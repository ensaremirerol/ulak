//! The app updates itself from the hub (Windows, macOS, Linux): same signed release and the same
//! `ulakd::update` code as the daemon, archive `ulak-<target>.tar.gz`. After an install the app
//! starts the new binary with the same arguments and exits; a new binary that dies within
//! `CONFIRM_AFTER_SECS` is replaced by the previous one on the next start.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::watch;
use ulak_proto::UpdateState;
use ulakd::update::{
    config::{Source, UpdateConfig},
    install, now_ms, trust, version_info, Outcome, Updater,
};

/// Start `exe` again with this process's arguments, then exit.
fn relaunch(exe: &Path) -> ! {
    match std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .spawn()
    {
        Ok(_) => std::process::exit(0),
        Err(e) => {
            eprintln!("ulak: cannot start the updated app ({e}); start it again by hand");
            std::process::exit(1)
        }
    }
}

/// Call first thing in `main`: finishes the rollback bookkeeping of a previous self-update.
pub fn on_start(dir: &Path) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    match install::startup(dir, &exe, now_ms()) {
        Ok(install::Startup::RolledBack { bad }) => {
            eprintln!(
                "ulak: version {bad} failed right after its update; restored the previous app"
            );
            relaunch(&exe);
        }
        Ok(install::Startup::Probation) => {
            let d = dir.to_owned();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(install::CONFIRM_AFTER_SECS));
                install::confirm(&d, &exe);
            });
        }
        Ok(install::Startup::Normal) => {}
        Err(e) => eprintln!("ulak: update marker: {e}"),
    }
}

pub struct SelfUpdate {
    rt: tokio::runtime::Runtime,
    up: Arc<Updater>,
    rx: watch::Receiver<Option<UpdateState>>,
}

impl SelfUpdate {
    /// `dir` holds the identity the app connects with (its own, or ulakd's).
    pub fn start(dir: &Path) -> Result<Self, String> {
        let exe: PathBuf = std::env::current_exe().map_err(|e| e.to_string())?;
        let cfg = UpdateConfig {
            source: Some(Source::Hub),
            ..UpdateConfig::default()
        };
        let mut up = Updater::new(
            cfg,
            false,
            None,
            version_info(),
            exe.clone(),
            dir.to_owned(),
            trust::release_key(),
        )
        .with_product("ulak");
        up.exit = Box::new(move |_| relaunch(&exe));
        up.restart_delay = Duration::from_millis(300);
        let up = Arc::new(up);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let rx = up.subscribe();
        Ok(Self { rt, up, rx })
    }

    pub fn version(&self) -> String {
        let v = self.up.info();
        format!("{} ({})", v.version(), v.git())
    }

    pub fn check(&self) {
        let up = Arc::clone(&self.up);
        self.rt.spawn(async move { up.on_check().await });
    }

    /// Download, verify, install, then relaunch (the process exits on success).
    pub fn apply(&self) {
        let up = Arc::clone(&self.up);
        self.rt.spawn(async move {
            if let Some(Ok(Outcome::Installed(_))) = up.run(true, false).await {
                up.restart().await;
            }
        });
    }

    pub fn state(&self) -> Option<UpdateState> {
        self.rx.borrow().clone()
    }

    /// True once per state change (for `poll` to redraw).
    pub fn changed(&mut self) -> bool {
        let c = self.rx.has_changed().unwrap_or(false);
        if c {
            self.rx.borrow_and_update();
        }
        c
    }
}
