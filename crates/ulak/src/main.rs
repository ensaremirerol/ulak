#[cfg(not(target_arch = "wasm32"))]
use rune::AppSpec;
#[cfg(not(target_arch = "wasm32"))]
use std::io::{BufRead, Write};
#[cfg(not(target_arch = "wasm32"))]
use ulak::scenes;
#[cfg(not(target_arch = "wasm32"))]
use ulak::{
    app::{poll, setup, App},
    net::config_dir,
    view::{on_key, view},
};

#[cfg(not(target_arch = "wasm32"))]
fn ask(q: &str) -> std::io::Result<String> {
    let var = if q.contains("fingerprint") {
        "ULAK_FINGERPRINT"
    } else {
        "ULAK_OTP"
    };
    if let Ok(v) = std::env::var(var) {
        return Ok(v);
    }
    print!("{q}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}

/// `ulak join <hub-url>`: enroll this machine as a UI device (the OTP comes from "Add device").
#[cfg(not(target_arch = "wasm32"))]
fn join(url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let dir = config_dir();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let id = rt.block_on(ulakd::join::join(url, &dir, &mut ask))?;
    println!("enrolled as {} (files in {})", id.as_str(), dir.display());
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("join") {
        let Some(url) = args.get(1) else {
            eprintln!("usage: ulak join <https://hub[:port]>");
            std::process::exit(2);
        };
        if let Err(e) = join(url) {
            eprintln!("ulak: {e}");
            std::process::exit(1);
        }
        return;
    }
    rune::run(AppSpec::<App> {
        scenes: &[
            ("mux", scenes::mux),
            ("questions", scenes::questions),
            ("spawn", scenes::spawn),
            ("mailbox", scenes::mailbox),
            ("devices", scenes::devices),
            ("updates", scenes::updates),
            ("busy", scenes::busy),
            ("blockers", scenes::blockers),
        ],
        on_key: Some(on_key),
        families: ("Inter", "ui-monospace"),
        setup: Some(setup),
        poll: Some(poll),
        ..AppSpec::new("ulak", (1000.0, 650.0), App::default, view)
    });
}

// The browser build is the library's `start` (src/web.rs); the bin target is empty there.
#[cfg(target_arch = "wasm32")]
fn main() {}
