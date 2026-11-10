use anyhow::{ensure, Context, Result};
use clap::Subcommand;
use sc_core::acoustics::{
    common_ceiling, select_ceiling, verify_repeated_knee, Candidate, NoiseWindow, Policy,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command as Exec, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

use crate::process::{output, Process};

#[derive(Subcommand)]
pub enum Command {
    /// Validate a quiet-policy Pkl file without hardware access
    ValidatePolicy {
        #[arg(long)]
        config: PathBuf,
    },
    /// Capture raw hardware microphone metrics as JSON lines; PCM stays in memory
    Capture {
        #[arg(long)]
        source: String,
        #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..=7200))]
        duration_seconds: u64,
    },
    /// Select a repeated acoustic/cooling knee from an ASUS benchmark report
    Analyze {
        #[arg(long)]
        report: PathBuf,
    },
    /// Persist the common measured joint ceiling across report scopes
    Envelope {
        #[arg(long, num_args=1.., required=true)]
        reports: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Measurement {
    pub schema_version: u32,
    pub source: String,
    pub setup: Value,
    pub elapsed_seconds: f64,
    pub monotonic_ns: u64,
    pub window: Option<NoiseWindow>,
    pub error: Option<String>,
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::ValidatePolicy { config } => {
            sc_core::quiet::load(&config)?;
            println!("Quiet policy schema and thermal/acoustic invariants are valid.");
            Ok(())
        }
        Command::Capture {
            source,
            duration_seconds,
        } => capture(&source, duration_seconds),
        Command::Analyze { report } => {
            let report: Value = serde_json::from_slice(&std::fs::read(report)?)?;
            let (candidates, policy) = validate_report(&report)?;
            let selected = select_ceiling(&candidates, &policy)?;
            println!(
                "{}",
                json!({"status":"candidate", "selected":selected, "qualification":"requires workload/profile/lifecycle qualification; dBFS is not dB(A)"})
            );
            Ok(())
        }
        Command::Envelope { reports, out } => {
            let mut cases = Vec::new();
            let mut loaded = Vec::new();
            for path in &reports {
                let report: Value = serde_json::from_slice(&std::fs::read(path)?)?;
                let (candidates, policy) = validate_report(&report)?;
                loaded.push((report, policy));
                cases.push(candidates);
            }
            let (first, policy) = &loaded[0];
            ensure!(
                loaded
                    .iter()
                    .all(|(r, p)| r["hardware"] == first["hardware"]
                        && r["baseline"]["setup"] == first["baseline"]["setup"]
                        && serde_json::to_value(p).ok() == serde_json::to_value(policy).ok()),
                "reports use different hardware, capture setup or selection policy"
            );
            let (cpu, gpu) = common_ceiling(&cases, policy)?;
            let envelope = json!({"schema_version":1,"status":"candidate","cpu_pwm_ceiling":cpu,"gpu_pwm_ceiling":gpu,
                "hardware":first["hardware"],"microphone":first["baseline"]["setup"],"reports":reports,
                "scopes":loaded.iter().map(|(r,_)|json!({"profile":r["profile"],"context":r["context"],"workload":r["workload"],"cpu_ppt":r["cpu_ppt"],"gpu_clock_mhz":r["gpu_clock_mhz"]})).collect::<Vec<_>>(),
                "qualification":"measured common ceiling; power, burst, lifecycle and performance acceptance required before runtime promotion"});
            use std::io::Write;
            let parent = out
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| std::path::Path::new("."));
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(&serde_json::to_vec_pretty(&envelope)?)?;
            file.as_file().sync_all()?;
            file.persist(out)?;
            println!("{}", envelope);
            Ok(())
        }
    }
}

fn validate_report(report: &Value) -> Result<(Vec<Candidate>, Policy)> {
    ensure!(
        report["schema_version"] == 1
            && report["benchmark_error"].is_null()
            && report["workload_stopped"] == true
            && report["curves_restored"] == true
            && report["power_restored"] == true,
        "failed, interrupted or unrestored benchmark cannot select a candidate"
    );
    let candidates: Vec<Candidate> = serde_json::from_value(report["candidates"].clone())?;
    let policy: Policy = serde_json::from_value(report["selection_policy"].clone())?;
    let selected = select_ceiling(&candidates, &policy)?;
    let repeats: Vec<Vec<Candidate>> =
        serde_json::from_value(report["repeated_candidates"].clone())?;
    let quiet = repeats
        .iter()
        .find(|r| {
            r.first()
                .is_some_and(|c| c.cpu_pwm == selected.cpu_pwm && c.gpu_pwm == selected.gpu_pwm)
        })
        .context("selected repetitions missing")?;
    let louder = repeats
        .iter()
        .filter(|r| {
            r.first().is_some_and(|c| {
                c.cpu_pwm >= selected.cpu_pwm
                    && c.gpu_pwm >= selected.gpu_pwm
                    && (c.cpu_pwm, c.gpu_pwm) != (selected.cpu_pwm, selected.gpu_pwm)
            })
        })
        .min_by_key(|r| u16::from(r[0].cpu_pwm) + u16::from(r[0].gpu_pwm))
        .context("louder repetitions missing")?;
    verify_repeated_knee(quiet, louder, &policy)?;
    Ok((candidates, policy))
}

fn source_setup(source: &str) -> Result<Value> {
    ensure!(source.starts_with("alsa_input.") && !source.ends_with(".monitor"), "an explicit raw ALSA input is required; processed/default/monitor sources cannot measure fan noise");
    let bytes = output(
        Exec::new("pactl").args(["--format=json", "list", "sources"]),
        Duration::from_secs(2),
    )?;
    let sources: Vec<Value> = serde_json::from_slice(&bytes)?;
    let source = sources
        .iter()
        .find(|s| s["name"].as_str() == Some(source))
        .context("raw microphone not found in this user's audio session")?;
    ensure!(
        source["mute"].as_bool() == Some(false),
        "microphone is muted"
    );
    ensure!(source["volume"].is_object(), "microphone gain unavailable");
    ensure!(
        source["active_port"]
            .as_str()
            .is_some_and(|port| port.contains("internal-mic")),
        "internal microphone port is not active"
    );
    Ok(
        json!({"name":source["name"], "volume":source["volume"], "active_port":source["active_port"], "sample_specification":source["sample_specification"], "device":source["properties"]["device.bus_path"]}),
    )
}

pub fn interrupt() -> Result<Arc<AtomicBool>> {
    let flag = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(signal, flag.clone())?;
    }
    Ok(flag)
}

fn capture(source: &str, duration_seconds: u64) -> Result<()> {
    let setup = source_setup(source)?;
    let flag = interrupt()?;
    let mut process = Process::spawn(
        Exec::new("parec")
            .args([
                "--raw",
                "--format=float32le",
                "--rate=48000",
                "--channels=1",
                "--latency-msec=100",
                "--volume=65536",
                "--client-name=SmartCool acoustic benchmark",
                "--device",
                source,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()),
    )?;
    let mut stdout = process.0.stdout.take().context("PCM stream unavailable")?;
    let (send, receive) = mpsc::sync_channel(2);
    std::thread::spawn(move || loop {
        let mut bytes = vec![0; 48000 * 4];
        let result = stdout
            .read_exact(&mut bytes)
            .map(|_| (bytes, monotonic_ns()));
        let done = result.is_err();
        if send.send(result).is_err() || done {
            break;
        }
    });
    let start = Instant::now();
    let mut last_setup = Instant::now();
    while start.elapsed() < Duration::from_secs(duration_seconds) && !flag.load(Ordering::Relaxed) {
        let (bytes, timestamp) = receive
            .recv_timeout(Duration::from_secs(3))
            .context("microphone capture heartbeat lost")??;
        ensure!(
            monotonic_ns().saturating_sub(timestamp) <= 1_500_000_000,
            "stale buffered microphone samples"
        );
        if last_setup.elapsed() >= Duration::from_secs(5) {
            ensure!(
                source_setup(source)? == setup,
                "microphone gain, mute, port or device changed; start a fresh measurement"
            );
            last_setup = Instant::now();
        }
        let samples: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let window = NoiseWindow::from_pcm(&samples);
        let measurement = Measurement {
            schema_version: 1,
            source: source.to_owned(),
            setup: setup.clone(),
            elapsed_seconds: start.elapsed().as_secs_f64(),
            monotonic_ns: timestamp,
            error: window.as_ref().err().map(ToString::to_string),
            window: window.ok(),
        };
        writeln!(
            std::io::stdout(),
            "{}",
            serde_json::to_string(&measurement)?
        )?;
        std::io::stdout().flush()?;
    }
    process.stop()
}

/// Argument prefix that runs a command in `user`'s desktop session.
pub fn as_user(user: &str) -> Result<Vec<String>> {
    ensure!(
        !user.is_empty()
            && user
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
        "invalid capture user"
    );
    let uid = output(Exec::new("id").args(["-u", user]), Duration::from_secs(2))?;
    let uid: u32 = std::str::from_utf8(&uid)?.trim().parse()?;
    ensure!(uid != 0, "capture user must have a desktop audio session");
    let mut prefix: Vec<String> = [
        "runuser",
        "-u",
        user,
        "--",
        "env",
        "-u",
        "PULSE_SERVER",
        "-u",
        "PULSE_SOURCE",
    ]
    .map(String::from)
    .into();
    prefix.push(format!("XDG_RUNTIME_DIR=/run/user/{uid}"));
    Ok(prefix)
}

pub struct Feed {
    _process: Process,
    receiver: mpsc::Receiver<Measurement>,
    setup: Option<Value>,
    last_elapsed: Option<f64>,
    pub latest: Option<Measurement>,
    arrival: Instant,
    source: String,
}

impl Feed {
    pub fn start(source: &str, user: Option<&str>, duration_seconds: u64) -> Result<Self> {
        let executable = std::env::current_exe()?;
        let mut command = if let Some(user) = user {
            let prefix = as_user(user)?;
            let mut command = Exec::new(&prefix[0]);
            command.args(&prefix[1..]).arg(executable);
            command
        } else {
            Exec::new(executable)
        };
        command.args([
            "acoustic",
            "capture",
            "--source",
            source,
            "--duration-seconds",
            &duration_seconds.to_string(),
        ]);
        let mut process = Process::spawn(command.stdout(Stdio::piped()).stderr(Stdio::inherit()))?;
        let stdout = process
            .0
            .stdout
            .take()
            .context("acoustic feed unavailable")?;
        let (send, receiver) = mpsc::sync_channel(2);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut bytes = Vec::new();
                match reader.by_ref().take(8193).read_until(b'\n', &mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if bytes.len() > 8192 => break,
                    _ => {}
                }
                let Ok(measurement) = serde_json::from_slice(&bytes) else {
                    break;
                };
                if send.send(measurement).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            _process: process,
            receiver,
            setup: None,
            last_elapsed: None,
            latest: None,
            arrival: Instant::now(),
            source: source.into(),
        })
    }

    /// Discard measurements queued while the caller was not sampling, then
    /// validate the next live one.
    pub fn resync(&mut self) -> Result<&Measurement> {
        // The reader thread forwards its own backlog right behind the
        // channel's: only a quiet gap means the queue is live again.
        while self
            .receiver
            .recv_timeout(Duration::from_millis(50))
            .is_ok()
        {}
        self.sample()
    }

    pub fn sample(&mut self) -> Result<&Measurement> {
        match self.receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(measurement) => {
                ensure!(
                    measurement.schema_version == 1 && measurement.elapsed_seconds.is_finite(),
                    "invalid acoustic feed schema/time"
                );
                ensure!(
                    measurement.source == self.source
                        && measurement.monotonic_ns <= monotonic_ns()
                        && monotonic_ns().saturating_sub(measurement.monotonic_ns) <= 1_500_000_000,
                    "stale or wrong-source acoustic feed"
                );
                ensure!(
                    self.last_elapsed
                        .is_none_or(|t| measurement.elapsed_seconds > t),
                    "nonmonotonic acoustic feed"
                );
                ensure!(
                    self.setup.as_ref().is_none_or(|s| *s == measurement.setup),
                    "acoustic setup changed"
                );
                ensure!(
                    measurement
                        .window
                        .as_ref()
                        .is_some_and(|w| w.level_dbfs.is_finite() && w.samples >= 4800),
                    "invalid acoustic window: {:?}",
                    measurement.error
                );
                self.setup = Some(measurement.setup.clone());
                self.last_elapsed = Some(measurement.elapsed_seconds);
                self.latest = Some(measurement);
                self.arrival = Instant::now();
            }
            Err(_) => anyhow::bail!(
                "acoustic capture lost (last heartbeat {:.1}s ago)",
                self.arrival.elapsed().as_secs_f64()
            ),
        }
        self.latest.as_ref().context("acoustic sample unavailable")
    }
}

pub fn monotonic_ns() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // CLOCK_MONOTONIC is shared by the privileged controller and capture user.
    let result = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) };
    if result != 0 {
        return 0;
    }
    (time.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(time.tv_nsec as u64)
}
