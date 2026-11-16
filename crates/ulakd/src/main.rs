use std::{
    io::{BufRead, Write},
    sync::{Arc, Mutex},
    time::Duration,
};
use ulakd::{
    config::{default_dir, Identity},
    hub::{run_forever_in, SharedProbe},
    join::{hostname, join},
    telemetry::SystemProbe,
    tmux::Tmux,
};

const USAGE: &str = "usage: ulakd join <https://hub[:port]> | ulakd run";

fn prompt(q: &str) -> std::io::Result<String> {
    print!("{q}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}

/// Non-interactive join: `ULAKD_FINGERPRINT` and `ULAKD_OTP` answer the two prompts; others read stdin.
fn answer(q: &str) -> std::io::Result<String> {
    let var = if q.contains("fingerprint") {
        "ULAKD_FINGERPRINT"
    } else {
        "ULAKD_OTP"
    };
    match std::env::var(var) {
        Ok(v) => Ok(v),
        Err(_) => prompt(q),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = default_dir();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["join", url] => {
            let id = join(url, &dir, &mut answer).await?;
            println!("enrolled as {} (files in {})", id.as_str(), dir.display());
            Ok(())
        }
        ["run"] => {
            let id = Identity::load(&dir)?;
            let probe: SharedProbe =
                Arc::new(Mutex::new(Box::new(SystemProbe::new(id.guard.clone()))));
            let host = Tmux::new(std::env::var("ULAKD_TMUX_SOCKET").ok());
            tokio::select! {
                r = run_forever_in(&id, Some(&dir), hostname(), Duration::from_secs(5), &host, probe) => { r?; }
                _ = tokio::signal::ctrl_c() => {}
            }
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}
