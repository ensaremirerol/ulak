//! Telemetry parsing. Every parser takes file contents or command output, so fixtures can test it.
use crate::{
    errors::TelemetryError,
    guard::{evaluate, GuardConfig},
};
use std::time::{SystemTime, UNIX_EPOCH};
use ulak_proto::{NodeState, Telemetry};

fn parse_err(what: &'static str, detail: impl ToString) -> TelemetryError {
    TelemetryError::Parse {
        what,
        detail: detail.to_string(),
    }
}

/// `/proc/loadavg` -> 1-minute load.
pub fn parse_loadavg(s: &str) -> Result<f32, TelemetryError> {
    s.split_whitespace()
        .next()
        .ok_or_else(|| parse_err("loadavg", "empty"))?
        .parse()
        .map_err(|e| parse_err("loadavg", e))
}

/// Aggregate CPU counters from the first line of `/proc/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    idle: u64,
    total: u64,
}

pub fn parse_cpu_stat(s: &str) -> Result<CpuTimes, TelemetryError> {
    let line = s
        .lines()
        .next()
        .filter(|l| l.starts_with("cpu "))
        .ok_or_else(|| parse_err("stat", "no aggregate cpu line"))?;
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8) // user nice system idle iowait irq softirq steal; guest is already in user
        .map(|f| f.parse().map_err(|e| parse_err("stat", e)))
        .collect::<Result<_, _>>()?;
    if v.len() < 5 {
        return Err(parse_err("stat", "too few fields"));
    }
    Ok(CpuTimes {
        idle: v[3] + v[4],
        total: v.iter().sum(),
    })
}

/// CPU busy percentage between two samples. No elapsed ticks -> 0.
pub fn cpu_pct(prev: CpuTimes, cur: CpuTimes) -> f32 {
    let total = cur.total.saturating_sub(prev.total);
    if total == 0 {
        return 0.0;
    }
    let idle = cur.idle.saturating_sub(prev.idle).min(total);
    ((total - idle) as f64 / total as f64 * 100.0) as f32
}

/// `/proc/meminfo` -> used percentage, `(MemTotal - MemAvailable) / MemTotal`.
pub fn parse_meminfo(s: &str) -> Result<f32, TelemetryError> {
    let field = |name: &str| -> Result<f64, TelemetryError> {
        s.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|r| r.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| parse_err("meminfo", format!("missing {name}")))
    };
    let (total, avail) = (field("MemTotal")?, field("MemAvailable")?);
    if total <= 0.0 {
        return Err(parse_err("meminfo", "MemTotal is zero"));
    }
    Ok(((total - avail.min(total)) / total * 100.0) as f32)
}

/// One utilisation number per line (nvidia-smi `--format=csv,noheader,nounits`, or the contents of
/// one or more `gpu_busy_percent` files joined by newlines). The busiest GPU wins; none -> `None`.
pub fn parse_gpu_percents(s: &str) -> Result<Option<f32>, TelemetryError> {
    let mut best: Option<f32> = None;
    for line in s.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let v: f32 = line.parse().map_err(|e| parse_err("gpu", e))?;
        best = Some(best.map_or(v, |b| b.max(v)));
    }
    Ok(best)
}

fn read(what: &'static str, path: &str) -> Result<String, TelemetryError> {
    std::fs::read_to_string(path).map_err(|source| TelemetryError::Io { what, source })
}

/// Best effort: nvidia-smi first, then AMD sysfs. Any failure means "no GPU reading".
pub fn read_gpu_pct() -> Option<f32> {
    let smi = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            parse_gpu_percents(&String::from_utf8_lossy(&o.stdout))
                .ok()
                .flatten()
        });
    if smi.is_some() {
        return smi;
    }
    let mut all = String::new();
    for e in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("card") && !name.contains('-') {
            if let Ok(s) = std::fs::read_to_string(e.path().join("device/gpu_busy_percent")) {
                all.push_str(&s);
                all.push('\n');
            }
        }
    }
    parse_gpu_percents(&all).ok().flatten()
}

/// Process names (`/proc/<pid>/comm`) of everything running.
pub fn read_process_names() -> Vec<String> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
        })
        .filter_map(|e| std::fs::read_to_string(e.path().join("comm")).ok())
        .map(|s| s.trim_end().to_owned())
        .collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the unix epoch")
        .as_millis() as u64
}

/// A telemetry reading and the node state derived from it.
#[derive(Debug, Clone, PartialEq)]
pub struct Probed {
    pub telemetry: Telemetry,
    pub state: NodeState,
}

pub trait Probe: Send {
    fn sample(&mut self) -> Result<Probed, TelemetryError>;
}

/// Reads the real system (`/proc`, nvidia-smi, sysfs).
pub struct SystemProbe {
    prev: Option<CpuTimes>,
    guard: GuardConfig,
    cores: u32,
}

impl SystemProbe {
    pub fn new(guard: GuardConfig) -> Self {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
        let prev = read("stat", "/proc/stat")
            .and_then(|s| parse_cpu_stat(&s))
            .ok();
        Self { prev, guard, cores }
    }
}

impl Probe for SystemProbe {
    fn sample(&mut self) -> Result<Probed, TelemetryError> {
        let cur = parse_cpu_stat(&read("stat", "/proc/stat")?)?;
        let cpu = self.prev.map_or(0.0, |p| cpu_pct(p, cur));
        self.prev = Some(cur);
        let telemetry = Telemetry::new(
            parse_loadavg(&read("loadavg", "/proc/loadavg")?)?,
            cpu,
            read_gpu_pct(),
            parse_meminfo(&read("meminfo", "/proc/meminfo")?)?,
            now_ms(),
        )?;
        let state = evaluate(&self.guard, &read_process_names(), &telemetry, self.cores);
        Ok(Probed { telemetry, state })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loadavg() {
        assert_eq!(parse_loadavg("0.52 0.40 0.30 1/500 1234\n").unwrap(), 0.52);
        assert!(parse_loadavg("").is_err());
        assert!(parse_loadavg("x 1 2").is_err());
    }

    #[test]
    fn cpu_delta() {
        let a = parse_cpu_stat("cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 1 2 3\n").unwrap();
        let b = parse_cpu_stat("cpu  200 0 200 750 150 0 0 0 0 0\n").unwrap();
        // total delta 300, idle delta 100 -> 66.67% busy
        assert!((cpu_pct(a, b) - 66.666).abs() < 1e-2);
        assert_eq!(cpu_pct(a, a), 0.0);
        assert!(parse_cpu_stat("cpu0 1 2 3 4 5\n").is_err());
        assert!(parse_cpu_stat("cpu  1 2 3\n").is_err());
    }

    #[test]
    fn meminfo() {
        let s = "MemTotal:       16000000 kB\nMemFree: 1 kB\nMemAvailable:    4000000 kB\n";
        assert!((parse_meminfo(s).unwrap() - 75.0).abs() < 1e-3);
        assert!(parse_meminfo("MemTotal: 1 kB\n").is_err());
        assert!(parse_meminfo("MemTotal: 0 kB\nMemAvailable: 0 kB\n").is_err());
    }

    #[test]
    fn gpu() {
        assert_eq!(parse_gpu_percents("12\n57\n").unwrap(), Some(57.0));
        assert_eq!(parse_gpu_percents("\n").unwrap(), None);
        assert!(parse_gpu_percents("N/A\n").is_err());
    }

    #[test]
    fn system_probe_reads_this_machine() {
        let p = SystemProbe::new(GuardConfig::default()).sample().unwrap();
        assert!(p.telemetry.mem_used_pct() > 0.0);
    }
}
