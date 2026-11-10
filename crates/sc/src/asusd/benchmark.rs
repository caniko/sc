use super::verify::fan_rpms;
use super::{read_curves, FanCurvesBusProxy, PlatformBusProxy, Profile, WireCurve};
use crate::acoustic::{as_user, interrupt, Feed, Measurement};
use crate::process::{output, Process};
use anyhow::{ensure, Context, Result};
use clap::Args;
use sc_core::acoustics::{
    equivalent_level, select_ceiling, verify_repeated_knee, Candidate, Policy,
};
use sc_hwmon::telemetry::{read_snapshot, Snapshot};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

#[derive(Args)]
pub struct ObservationOptions {
    #[arg(long)]
    microphone: String,
    #[arg(long)]
    capture_user: Option<String>,
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u64).range(1..=7200))]
    duration_seconds: u64,
    /// Relative dBFS limit from a qualification with this exact capture setup
    #[arg(long)]
    maximum_noise_dbfs: Option<f64>,
}

pub fn observe(
    platform: &PlatformBusProxy<'_>,
    fans: &FanCurvesBusProxy<'_>,
    options: ObservationOptions,
) -> Result<()> {
    ensure!(
        options
            .maximum_noise_dbfs
            .is_none_or(|x| x.is_finite() && x <= 0.0),
        "invalid digital noise ceiling"
    );
    let runtime = super::discover(platform, fans)?;
    let mut feed = Feed::start(
        &options.microphone,
        options.capture_user.as_deref(),
        options.duration_seconds,
    )?;
    let start = Instant::now();
    let flag = interrupt()?;
    let mut previous = std::collections::BTreeMap::<String, u32>::new();
    let mut starts = std::collections::BTreeMap::<String, u64>::new();
    let mut maximum_rpm = std::collections::BTreeMap::<String, u32>::new();
    let mut levels = Vec::new();
    let mut violations = 0;
    for _ in 0..options.duration_seconds {
        if flag.load(Ordering::Relaxed) {
            break;
        }
        let acoustic = feed.sample()?.clone();
        let profile = Profile::from_id(platform.platform_profile()?)?;
        let snapshot = read_snapshot(Path::new("/sys/class/hwmon"))?;
        let current = super::RuntimeState {
            active: profile,
            profiles: runtime.profiles.clone(),
            curves: runtime.curves.clone(),
        };
        let rpms = fan_rpms(&current, &snapshot)?;
        for (name, rpm) in &rpms {
            if previous.get(name).is_some_and(|old| *old == 0) && *rpm > 0 {
                *starts.entry(name.clone()).or_default() += 1;
            }
            let maximum = maximum_rpm.entry(name.clone()).or_default();
            *maximum = (*maximum).max(*rpm);
        }
        previous = rpms.clone();
        let level = acoustic
            .window
            .as_ref()
            .context("microphone window unavailable")?
            .level_dbfs;
        levels.push(level);
        violations += usize::from(
            options
                .maximum_noise_dbfs
                .is_some_and(|limit| level > limit),
        );
        writeln!(
            std::io::stdout(),
            "{}",
            json!({"type":"acoustic_observation", "elapsed_seconds":start.elapsed().as_secs_f64(), "profile":profile.name(), "rpm":rpms, "acoustic":acoustic, "temperatures":snapshot.temperatures})
        )?;
    }
    println!(
        "{}",
        json!({"type":"summary", "samples":levels.len(), "equivalent_level_dbfs":equivalent_level(&levels)?, "maximum_window_dbfs":levels.iter().copied().fold(f64::NEG_INFINITY,f64::max), "fan_starts":starts, "maximum_rpm":maximum_rpm, "noise_violations":violations,"completed":levels.len()==options.duration_seconds as usize})
    );
    ensure!(
        levels.len() == options.duration_seconds as usize,
        "acoustic observation interrupted"
    );
    ensure!(
        violations == 0,
        "observed noise exceeded the supplied relative ceiling in {violations} windows"
    );
    Ok(())
}

#[derive(Args)]
pub struct Options {
    #[arg(long)]
    pub microphone: String,
    /// Desktop-session user for the unprivileged capture child
    #[arg(long)]
    pub capture_user: Option<String>,
    /// Joint PWM pairs, in ascending order; never more than 200/255
    #[arg(long, default_value = "40:40,70:70,100:100,130:130,160:160,190:190")]
    pub levels: String,
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(3..=5))]
    pub repetitions: u32,
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(30..=180))]
    pub window_seconds: u64,
    #[arg(long, default_value_t = 180, value_parser = clap::value_parser!(u64).range(60..=300))]
    pub settle_seconds: u64,
    #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(60..=7200))]
    pub duration_seconds: u64,
    /// Required NVIDIA PCI BDF when the workload exercises the discrete GPU
    #[arg(long)]
    pub gpu: Option<String>,
    /// Require confirmed sustained NVIDIA utilization (GPU/combined workload)
    #[arg(long, requires = "gpu")]
    pub require_gpu_load: bool,
    #[arg(long, default_value = "/sys/class/powercap/intel-rapl:0")]
    pub energy_path: PathBuf,
    #[arg(long, default_value_t = 85.0)]
    pub cpu_stop_c: f64,
    #[arg(long, default_value_t = 70.0)]
    pub gpu_stop_c: f64,
    #[arg(long)]
    pub report: PathBuf,
    /// Optional workload-produced monotonically increasing completed-work counter
    #[arg(long)]
    pub throughput_counter: Option<PathBuf>,
    /// Physical/power setup identity (for example atlas-adjacent-ac-lid-open)
    #[arg(long)]
    pub context: String,
    /// Trial ASUS power requests, restored on exit (PL1,PL2,PL3 watts)
    #[arg(long)]
    pub cpu_ppt: Option<String>,
    /// Exclusive NVIDIA clock ownership for this trial; reset to driver defaults on exit
    #[arg(long, requires = "gpu", value_parser = clap::value_parser!(u32).range(210..=1800))]
    pub gpu_clock_mhz: Option<u32>,
    /// Stop the workload while AMC admission requests (of the capture user,
    /// when given) wait for capacity; restart it and the interrupted window
    /// once they finish. The optional value is the amc executable
    #[arg(long, num_args = 0..=1, default_missing_value = "amc", value_name = "AMC")]
    pub yield_to_amc: Option<String>,
    /// Preview by default. --apply starts the supplied workload and changes curves
    #[arg(long)]
    pub apply: bool,
    /// Direct executable and arguments; no shell interpretation
    #[arg(last = true, required = true)]
    pub workload: Vec<String>,
}

#[derive(Clone, Serialize)]
pub struct Thermal {
    pub cpu_c: f64,
    pub gpu_c: Option<f64>,
    pub cpu_w: f64,
    pub gpu_w: Option<f64>,
    pub gpu_utilization: Option<f64>,
    pub gpu_clock_mhz: Option<f64>,
}

pub struct Energy {
    path: PathBuf,
    maximum: u64,
    previous: u64,
    time: Instant,
}

impl Energy {
    pub fn new(path: &Path) -> Result<Self> {
        let read =
            |name| -> Result<u64> { Ok(std::fs::read_to_string(path.join(name))?.trim().parse()?) };
        let maximum = read("max_energy_range_uj")?;
        ensure!(maximum > 0, "invalid package energy range");
        Ok(Self {
            path: path.into(),
            maximum,
            previous: read("energy_uj")?,
            time: Instant::now(),
        })
    }

    pub fn watts(&mut self) -> Result<f64> {
        let now: u64 = std::fs::read_to_string(self.path.join("energy_uj"))?
            .trim()
            .parse()?;
        let elapsed = self.time.elapsed().as_secs_f64();
        ensure!(
            elapsed >= 0.1 && now <= self.maximum && self.previous <= self.maximum,
            "invalid package energy sample"
        );
        let delta = if now >= self.previous {
            now - self.previous
        } else {
            self.maximum - self.previous + now
        };
        self.previous = now;
        self.time = Instant::now();
        let watts = delta as f64 / 1_000_000.0 / elapsed;
        ensure!(
            watts.is_finite() && (0.0..=200.0).contains(&watts),
            "implausible CPU package power"
        );
        Ok(watts)
    }
}

pub fn gpu_active(id: &str) -> Result<bool> {
    sc_core::quiet::validate_pci_id(id)?;
    let status =
        std::fs::read_to_string(format!("/sys/bus/pci/devices/{id}/power/runtime_status"))?;
    match status.trim() {
        "active" => Ok(true),
        "suspended" => Ok(false),
        other => anyhow::bail!("unknown/transitioning GPU runtime state: {other}"),
    }
}

pub fn thermal(snapshot: &Snapshot, energy: &mut Energy, gpu: Option<&str>) -> Result<Thermal> {
    let cpu_c = cpu_temperature(snapshot)?;
    let mut reading = Thermal {
        cpu_c,
        cpu_w: energy.watts()?,
        gpu_c: None,
        gpu_w: None,
        gpu_utilization: None,
        gpu_clock_mhz: None,
    };
    if let Some(id) = gpu {
        ensure!(gpu_active(id)?, "benchmark GPU is not active");
        let bytes = output(
            Command::new("nvidia-smi").args([
                "-i",
                id,
                "--query-gpu=temperature.gpu,power.draw,utilization.gpu,clocks.current.graphics",
                "--format=csv,noheader,nounits",
            ]),
            Duration::from_secs(1),
        )?;
        let fields: Vec<f64> = std::str::from_utf8(&bytes)?
            .trim()
            .split(',')
            .map(|x| x.trim().parse())
            .collect::<std::result::Result<_, _>>()?;
        ensure!(
            fields.len() == 4 && fields.iter().all(|x| x.is_finite()),
            "invalid NVIDIA telemetry"
        );
        ensure!(
            (0.0..=110.0).contains(&fields[0])
                && (0.0..=200.0).contains(&fields[1])
                && (0.0..=100.0).contains(&fields[2]),
            "implausible NVIDIA telemetry"
        );
        reading.gpu_c = Some(fields[0]);
        reading.gpu_w = Some(fields[1]);
        reading.gpu_utilization = Some(fields[2]);
        ensure!(
            (0.0..=4000.0).contains(&fields[3]),
            "implausible NVIDIA clock"
        );
        reading.gpu_clock_mhz = Some(fields[3]);
    }
    Ok(reading)
}

pub fn cpu_temperature(snapshot: &Snapshot) -> Result<f64> {
    let cpu: Vec<_> = snapshot
        .temperatures
        .iter()
        .filter(|s| s.chip == "k10temp" && s.label.as_deref() == Some("Tctl"))
        .collect();
    ensure!(cpu.len() == 1, "CPU Tctl unavailable or ambiguous");
    let value = cpu[0].temp_c.context("CPU temperature unreadable")?;
    ensure!(
        value.is_finite() && (0.0..=120.0).contains(&value),
        "implausible CPU temperature"
    );
    Ok(value)
}

pub fn gpu_temperature(id: &str) -> Result<f64> {
    let bytes = output(
        Command::new("nvidia-smi").args([
            "-i",
            id,
            "--query-gpu=temperature.gpu",
            "--format=csv,noheader,nounits",
        ]),
        Duration::from_secs(1),
    )?;
    let temp: f64 = std::str::from_utf8(&bytes)?.trim().parse()?;
    ensure!(
        temp.is_finite() && (0.0..=110.0).contains(&temp),
        "invalid NVIDIA temperature"
    );
    Ok(temp)
}

struct Curves<'a> {
    bus: &'a FanCurvesBusProxy<'a>,
    profile: Profile,
    original: Vec<WireCurve>,
    expected: Vec<WireCurve>,
    restored: bool,
}

impl Curves<'_> {
    fn set(&mut self, pair: (u8, u8)) -> Result<()> {
        ensure!(
            same_curves(&read_curves(self.bus, self.profile)?, &self.expected),
            "another writer changed firmware curves; aborting experiment"
        );
        for original in &self.original {
            let mut curve = original.clone();
            let cap = match curve.0.as_str() {
                "CPU" => pair.0,
                "GPU" => pair.1,
                _ => anyhow::bail!("unsupported benchmark fan"),
            };
            // A complete temporary table, preserving the original emergency
            // endpoint. Benchmark guards stop before interpolation into it.
            curve.1 = [cap; 8];
            curve.1[7] = 255;
            curve.3 = true;
            self.expected.retain(|old| old.0 != curve.0);
            self.expected.push(curve.clone());
            self.bus.set_fan_curve(self.profile.id(), curve)?;
        }
        ensure!(
            same_curves(&read_curves(self.bus, self.profile)?, &self.expected),
            "temporary curve storage mismatch"
        );
        Ok(())
    }

    fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        let current = read_curves(self.bus, self.profile)?;
        let mut errors = Vec::new();
        for original in &self.original {
            let actual = current.iter().find(|c| c.0 == original.0);
            let expected = self.expected.iter().find(|c| c.0 == original.0);
            if actual != expected && actual != Some(original) {
                errors.push(format!(
                    "{} changed by another writer; not overwriting its newer curve",
                    original.0
                ));
            } else if let Err(error) = self.bus.set_fan_curve(self.profile.id(), original.clone()) {
                errors.push(error.to_string());
            }
        }
        ensure!(
            errors.is_empty(),
            "curve restoration failed: {}",
            errors.join("; ")
        );
        ensure!(
            same_curves(&read_curves(self.bus, self.profile)?, &self.original),
            "original curve restoration mismatch"
        );
        self.restored = true;
        Ok(())
    }
}

impl Drop for Curves<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.restore() {
            eprintln!("ASUS recovery required: {e:#}");
        }
    }
}

pub fn same_curves(left: &[WireCurve], right: &[WireCurve]) -> bool {
    left.len() == right.len() && left.iter().all(|c| right.contains(c))
}

#[derive(Clone, Serialize)]
struct Sample {
    elapsed_seconds: f64,
    thermal: Thermal,
    cpu_rpm: u32,
    gpu_rpm: u32,
    acoustic: Measurement,
    completed_work: Option<u64>,
}

type Trials = Vec<((u8, u8), Vec<Vec<Sample>>)>;

fn levels(value: &str) -> Result<Vec<(u8, u8)>> {
    let mut result: Vec<(u8, u8)> = Vec::new();
    for pair in value.split(',') {
        let (cpu, gpu) = pair
            .split_once(':')
            .context("levels must be CPU:GPU pairs")?;
        let pair = (cpu.parse::<u8>()?, gpu.parse::<u8>()?);
        ensure!(
            (1..=200).contains(&pair.0) && (1..=200).contains(&pair.1),
            "normal calibration PWM must be 1..200"
        );
        if let Some(previous) = result.last() {
            ensure!(
                pair.0 >= previous.0 && pair.1 >= previous.1 && pair != *previous,
                "joint levels must strictly progress without reducing either fan"
            );
        }
        result.push(pair);
    }
    ensure!(
        (2..=8).contains(&result.len()),
        "two to eight joint levels required"
    );
    Ok(result)
}

fn slope(samples: &[Sample], gpu: bool) -> f64 {
    let pairs: Vec<_> = samples
        .iter()
        .filter_map(|s| {
            Some((
                s.elapsed_seconds / 60.0,
                if gpu {
                    s.thermal.gpu_c?
                } else {
                    s.thermal.cpu_c
                },
            ))
        })
        .collect();
    let n = pairs.len() as f64;
    if n < 2.0 {
        return f64::INFINITY;
    }
    let x = pairs.iter().map(|p| p.0).sum::<f64>() / n;
    let y = pairs.iter().map(|p| p.1).sum::<f64>() / n;
    let denominator = pairs.iter().map(|p| (p.0 - x).powi(2)).sum::<f64>();
    if denominator <= 0.0 {
        return f64::INFINITY;
    }
    pairs.iter().map(|p| (p.0 - x) * (p.1 - y)).sum::<f64>() / denominator
}

#[derive(serde::Deserialize)]
struct Ticket {
    id: String,
}

#[derive(serde::Deserialize)]
struct Waiting {
    reason: String,
}

#[derive(serde::Deserialize)]
struct Admission {
    entries: Vec<Ticket>,
    waiting: BTreeMap<String, Waiting>,
}

/// From `amc admission status --json`: the requests waiting for capacity the
/// workload may hold, and every request that has not finished.
fn contenders(status: &[u8]) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let status: Admission = serde_json::from_slice(status).context("parse AMC status")?;
    Ok((
        status
            .waiting
            .into_iter()
            // Stopping the workload relieves neither a pause marker nor a
            // missing native manager; a FIFO follower's head speaks for it.
            .filter(|(_, w)| !matches!(w.reason.as_str(), "paused" | "native_unavailable" | "fifo"))
            .map(|(id, _)| id)
            .collect(),
        status.entries.into_iter().map(|e| e.id).collect(),
    ))
}

fn admission(argv: &[String]) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let status = output(
        Command::new(&argv[0]).args(&argv[1..]),
        Duration::from_secs(2),
    )
    .context("query AMC admission")?;
    contenders(&status)
}

fn spawn(workload: &[String]) -> Result<Process> {
    Process::spawn(
        Command::new(&workload[0])
            .args(&workload[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit()),
    )
}

struct Session<'a> {
    // Rust drops fields in declaration order, including during unwinding.
    // Stop heat generation before restoring curves on every exit path.
    workload: Process,
    platform: &'a PlatformBusProxy<'a>,
    curves: Curves<'a>,
    heat: super::quiet::HeatTrial,
    feed: Feed,
    energy: Energy,
    options: &'a Options,
    flag: Arc<AtomicBool>,
    started: Instant,
    all_samples: Vec<Sample>,
    last_counter: Option<u64>,
    amc: Option<Vec<String>>,
    yields: u32,
}

impl Session<'_> {
    /// Give the workload's compute to AMC requests waiting for capacity and
    /// take it back once they finish. Returns whether the workload restarted.
    fn yield_to_amc(&mut self) -> Result<bool> {
        let Some(argv) = self.amc.clone() else {
            return Ok(false);
        };
        let (mut served, _) = admission(&argv)?;
        if served.is_empty() {
            return Ok(false);
        }
        self.workload.stop()?;
        println!("{}", json!({"type":"amc_yield", "requests":served}));
        while !served.is_empty() {
            ensure!(!self.flag.load(Ordering::Relaxed), "benchmark interrupted");
            ensure!(
                self.started.elapsed() < Duration::from_secs(self.options.duration_seconds),
                "benchmark deadline reached while yielding to AMC"
            );
            // Admitted work runs under the trial curve: keep the thermal
            // guard. The sleep spaces this energy read from the next one,
            // here or in the restarted window; capture may be backlogged.
            self.guard()?;
            std::thread::sleep(Duration::from_secs(1));
            self.feed.resync()?;
            let (waiting, live) = admission(&argv)?;
            served.extend(waiting);
            served.retain(|id| live.contains(id));
        }
        self.workload = spawn(&self.options.workload)?;
        // A restarted workload may restart its counter.
        self.last_counter = None;
        self.yields += 1;
        println!("{}", json!({"type":"amc_resume"}));
        Ok(true)
    }

    fn guard(&mut self) -> Result<(Snapshot, Thermal)> {
        let snapshot = read_snapshot(Path::new("/sys/class/hwmon"))?;
        let active = self
            .options
            .gpu
            .as_deref()
            .map(gpu_active)
            .transpose()?
            .unwrap_or(false);
        let thermal = thermal(
            &snapshot,
            &mut self.energy,
            self.options.gpu.as_deref().filter(|_| active),
        )?;
        ensure!(
            thermal.cpu_c < self.options.cpu_stop_c
                && thermal.gpu_c.is_none_or(|t| t < self.options.gpu_stop_c),
            "thermal guard reached: CPU {:.1} C, GPU {:?}",
            thermal.cpu_c,
            thermal.gpu_c
        );
        Ok((snapshot, thermal))
    }

    fn sample(&mut self, pair: (u8, u8), spinup_grace: bool) -> Result<Option<Sample>> {
        ensure!(!self.flag.load(Ordering::Relaxed), "benchmark interrupted");
        ensure!(
            self.started.elapsed() < Duration::from_secs(self.options.duration_seconds),
            "benchmark deadline reached"
        );
        if self.yield_to_amc()? {
            return Ok(None);
        }
        ensure!(
            !self.workload.has_exited()?,
            "benchmark workload exited before qualification completed"
        );
        ensure!(
            Profile::from_id(self.platform.platform_profile()?)? == self.curves.profile,
            "profile changed; stop and start a new experiment"
        );
        let (snapshot, thermal) = self.guard()?;
        ensure!(
            !self.options.require_gpu_load || thermal.gpu_c.is_some(),
            "benchmark GPU is not active"
        );
        ensure!(
            self.options
                .gpu_clock_mhz
                .zip(thermal.gpu_clock_mhz)
                .is_none_or(|(cap, clock)| clock <= f64::from(cap) + 15.0),
            "NVIDIA clock request ineffective"
        );
        let runtime = super::RuntimeState {
            active: self.curves.profile,
            profiles: vec![self.curves.profile],
            curves: [(self.curves.profile, self.curves.expected.clone())].into(),
        };
        let rpms = fan_rpms(&runtime, &snapshot)?;
        let cpu_rpm = *rpms.get("CPU").context("CPU fan missing")?;
        let gpu_rpm = *rpms.get("GPU").context("GPU fan missing")?;
        ensure!(
            spinup_grace || (cpu_rpm > 0 && gpu_rpm > 0),
            "requested nonzero PWM but a fan did not start"
        );
        ensure!(
            same_curves(
                &read_curves(self.curves.bus, self.curves.profile)?,
                &self.curves.expected
            ),
            "firmware curve ownership changed"
        );
        for curve in &self.curves.expected {
            let index = if curve.0 == "CPU" { 1 } else { 2 };
            let actual = snapshot
                .curves
                .iter()
                .find(|c| c.index == index)
                .context("active driver curve unavailable")?;
            ensure!(
                actual.pwm == Some(curve.1)
                    && actual.temp == Some(curve.2)
                    && actual.enabled == Some(true),
                "active driver curve differs from trial"
            );
        }
        let acoustic = self.feed.sample()?.clone();
        let completed_work = self
            .options
            .throughput_counter
            .as_ref()
            .map(|path| -> Result<u64> {
                use std::io::Read;
                let mut bytes = Vec::new();
                std::fs::File::open(path)?
                    .take(33)
                    .read_to_end(&mut bytes)?;
                ensure!(bytes.len() <= 32, "oversized throughput counter");
                Ok(std::str::from_utf8(&bytes)?.trim().parse()?)
            })
            .transpose()?;
        ensure!(
            completed_work
                .zip(self.last_counter)
                .is_none_or(|(now, old)| now >= old),
            "throughput counter decreased"
        );
        self.last_counter = completed_work;
        let sample = Sample {
            elapsed_seconds: self.started.elapsed().as_secs_f64(),
            thermal,
            cpu_rpm,
            gpu_rpm,
            acoustic,
            completed_work,
        };
        writeln!(
            std::io::stdout(),
            "{}",
            json!({"type":"sample", "cpu_pwm":pair.0, "gpu_pwm":pair.1, "sample":sample})
        )?;
        self.all_samples.push(sample.clone());
        Ok(Some(sample))
    }

    fn window(&mut self, pair: (u8, u8)) -> Result<Vec<Sample>> {
        loop {
            // A yield invalidates the thermal state: settle and measure again.
            if let Some(samples) = self.attempt(pair)? {
                return Ok(samples);
            }
        }
    }

    fn attempt(&mut self, pair: (u8, u8)) -> Result<Option<Vec<Sample>>> {
        let start = Instant::now();
        let mut settling = Vec::new();
        loop {
            let Some(sample) = self.sample(pair, start.elapsed() < Duration::from_secs(10))? else {
                return Ok(None);
            };
            settling.push(sample);
            if settling.len() > self.options.window_seconds as usize {
                settling.remove(0);
            }
            if settling.len() >= self.options.window_seconds as usize {
                let cpu_slope = slope(&settling, false).abs();
                let gpu_slope = if settling.iter().any(|s| s.thermal.gpu_c.is_some()) {
                    slope(&settling, true).abs()
                } else {
                    0.0
                };
                if cpu_slope.max(gpu_slope) <= 0.2 {
                    break;
                }
            }
            ensure!(
                start.elapsed() < Duration::from_secs(self.options.settle_seconds),
                "temperature did not settle; trial is inconclusive"
            );
        }
        let mut samples = Vec::new();
        for _ in 0..self.options.window_seconds {
            let Some(sample) = self.sample(pair, false)? else {
                return Ok(None);
            };
            samples.push(sample);
        }
        let gpu_active = samples[0].thermal.gpu_c.is_some();
        ensure!(
            samples
                .iter()
                .all(|s| s.thermal.gpu_c.is_some() == gpu_active),
            "GPU runtime state changed within a measurement window"
        );
        ensure!(
            slope(&samples, false).abs() <= 0.2
                && (!gpu_active || slope(&samples, true).abs() <= 0.2),
            "measurement window drifted; trial is inconclusive"
        );
        if self.options.require_gpu_load {
            ensure!(
                samples
                    .iter()
                    .filter(|s| s.thermal.gpu_utilization.is_some_and(|u| u >= 50.0))
                    .count()
                    * 2
                    > samples.len(),
                "GPU workload was not confirmed active"
            );
        }
        Ok(Some(samples))
    }
}

pub fn run<'a>(
    platform: &'a PlatformBusProxy<'a>,
    fans: &'a FanCurvesBusProxy<'a>,
    options: Options,
) -> Result<()> {
    ensure!(
        !options.context.trim().is_empty(),
        "physical/power context must be recorded"
    );
    let levels = levels(&options.levels)?;
    let policy = Policy {
        cpu_limit_c: options.cpu_stop_c,
        gpu_limit_c: options.gpu_stop_c,
        ..Policy::default()
    };
    let cpu_ppt = options
        .cpu_ppt
        .as_ref()
        .map(|value| -> Result<[i32; 3]> {
            let values: Vec<i32> = value
                .split(',')
                .map(|x| x.parse())
                .collect::<std::result::Result<_, _>>()?;
            let values: [i32; 3] = values
                .try_into()
                .map_err(|_| anyhow::anyhow!("CPU PPT needs PL1,PL2,PL3"))?;
            ensure!(
                values.windows(2).all(|p| p[0] <= p[1]),
                "CPU PPT requests must be ordered"
            );
            Ok(values)
        })
        .transpose()?;
    policy.validate()?;
    let amc = options
        .yield_to_amc
        .as_ref()
        .map(|amc| -> Result<Vec<String>> {
            let mut argv = options
                .capture_user
                .as_deref()
                .map(as_user)
                .transpose()?
                .unwrap_or_default();
            argv.extend([amc.as_str(), "admission", "status", "--json"].map(String::from));
            // Fail before any hardware mutation if the coordinator is unreachable.
            admission(&argv)?;
            Ok(argv)
        })
        .transpose()?;
    let runtime = super::discover(platform, fans)?;
    let originals = runtime.curves[&runtime.active].clone();
    ensure!(
        originals.len() == 2
            && originals.iter().any(|c| c.0 == "CPU")
            && originals.iter().any(|c| c.0 == "GPU"),
        "exactly CPU and GPU ASUS fans required"
    );
    for curve in &originals {
        let stop = if curve.0 == "CPU" {
            options.cpu_stop_c
        } else {
            options.gpu_stop_c
        };
        ensure!(
            curve.1[7] == 255 && stop <= f64::from(curve.2[6]),
            "benchmark guard must precede the original emergency interpolation band for {}",
            curve.0
        );
    }
    println!(
        "{}",
        json!({"type":"plan", "profile":runtime.active.name(), "levels":levels, "microphone":options.microphone, "capture_user":options.capture_user, "workload":options.workload, "maximum_seconds":options.duration_seconds, "cpu_stop_c":options.cpu_stop_c, "gpu_stop_c":options.gpu_stop_c, "apply":options.apply})
    );
    if !options.apply {
        return Ok(());
    }
    let _lease = super::quiet::Lease::acquire()?;
    // Rediscover under the lease; the preview snapshot is not mutation authority.
    ensure!(
        Profile::from_id(platform.platform_profile()?)? == runtime.active
            && same_curves(&read_curves(fans, runtime.active)?, &originals),
        "profile/curves changed since preview"
    );
    // Never ignore an active NVIDIA device during a CPU-only experiment.
    for device in std::fs::read_dir("/sys/bus/pci/devices")? {
        let device = device?;
        let vendor = std::fs::read_to_string(device.path().join("vendor"))?;
        let class = std::fs::read_to_string(device.path().join("class"))?;
        if vendor.trim() == "0x10de" && class.trim().starts_with("0x03") {
            let id = device.file_name().to_string_lossy().into_owned();
            ensure!(
                options.gpu.as_deref() == Some(&id),
                "NVIDIA device {id} must be monitored with --gpu, including CPU-only experiments"
            );
        }
    }
    let flag = interrupt()?;
    let mut feed = Feed::start(
        &options.microphone,
        options.capture_user.as_deref(),
        options.duration_seconds,
    )?;
    let energy = Energy::new(&options.energy_path)?;
    let baseline = feed.sample()?.clone();
    let snapshot = read_snapshot(Path::new("/sys/class/hwmon"))?;
    let cpu = cpu_temperature(&snapshot)?;
    ensure!(cpu < options.cpu_stop_c, "CPU already at benchmark guard");
    if let Some(id) = &options.gpu {
        if gpu_active(id)? {
            ensure!(
                gpu_temperature(id)? < options.gpu_stop_c,
                "GPU already at benchmark guard"
            );
        }
    }
    let heat =
        super::quiet::HeatTrial::begin(cpu_ppt, options.gpu.as_deref(), options.gpu_clock_mhz)?;
    let mut curves = Curves {
        bus: fans,
        profile: runtime.active,
        original: originals.clone(),
        expected: originals,
        restored: false,
    };
    curves.set(levels[0])?;
    let workload = spawn(&options.workload)?;
    let mut session = Session {
        platform,
        curves,
        heat,
        feed,
        energy,
        options: &options,
        flag,
        started: Instant::now(),
        workload,
        all_samples: Vec::new(),
        last_counter: None,
        amc,
        yields: 0,
    };
    let mut measurements: Trials = levels.iter().map(|pair| (*pair, Vec::new())).collect();
    let result = (|| -> Result<()> {
        for repetition in 0..options.repetitions {
            // Reverse alternate sweeps to expose thermal/order drift.
            let order: Vec<_> = if repetition % 2 == 0 {
                (0..levels.len()).collect()
            } else {
                (0..levels.len()).rev().collect()
            };
            for index in order {
                session.curves.set(levels[index])?;
                let window = session.window(levels[index])?;
                measurements[index].1.push(window);
            }
        }
        Ok(())
    })();
    let workload_restore = session.workload.stop();
    let curve_restore = session.curves.restore();
    let heat_restore = session.heat.restore();
    let mut candidates = Vec::new();
    for (pair, windows) in &measurements {
        if windows.is_empty() {
            continue;
        }
        let samples: Vec<_> = windows.iter().flatten().collect();
        let n = samples.len() as f64;
        let average = |get: fn(&Sample) -> f64| samples.iter().map(|s| get(s)).sum::<f64>() / n;
        let noises: Vec<_> = windows
            .iter()
            .map(|w| {
                equivalent_level(
                    &w.iter()
                        .map(|s| s.acoustic.window.as_ref().unwrap().level_dbfs)
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Result<_>>()?;
        let powers: Vec<_> = windows
            .iter()
            .map(|w| {
                w.iter()
                    .map(|s| s.thermal.cpu_w + s.thermal.gpu_w.unwrap_or(0.0))
                    .sum::<f64>()
                    / w.len() as f64
            })
            .collect();
        let span = |v: &[f64]| {
            v.iter().copied().fold(f64::NEG_INFINITY, f64::max)
                - v.iter().copied().fold(f64::INFINITY, f64::min)
        };
        candidates.push(Candidate {
            cpu_pwm: pair.0,
            gpu_pwm: pair.1,
            noise_dbfs: equivalent_level(&noises)?,
            cpu_temp_c: average(|s| s.thermal.cpu_c),
            gpu_temp_c: samples
                .iter()
                .all(|s| s.thermal.gpu_c.is_some())
                .then(|| average(|s| s.thermal.gpu_c.unwrap())),
            cpu_rpm: Some(average(|s| f64::from(s.cpu_rpm)).round() as u32),
            gpu_rpm: Some(average(|s| f64::from(s.gpu_rpm)).round() as u32),
            power_w: average(|s| s.thermal.cpu_w + s.thermal.gpu_w.unwrap_or(0.0)),
            cpu_power_w: Some(average(|s| s.thermal.cpu_w)),
            gpu_power_w: samples
                .iter()
                .all(|s| s.thermal.gpu_w.is_some())
                .then(|| average(|s| s.thermal.gpu_w.unwrap())),
            slope_c_per_min: windows
                .iter()
                .map(|w| {
                    slope(w, false)
                        .abs()
                        .max(if w.iter().all(|s| s.thermal.gpu_c.is_some()) {
                            slope(w, true).abs()
                        } else {
                            0.0
                        })
                })
                .fold(0.0, f64::max),
            repetitions: windows.len() as u32,
            valid: span(&noises) <= 1.5
                && span(&powers) / (powers.iter().sum::<f64>() / powers.len() as f64) <= 0.05
                && samples
                    .iter()
                    .all(|s| s.thermal.gpu_c.is_some() == samples[0].thermal.gpu_c.is_some()),
        });
    }
    let repeated: Vec<Vec<Candidate>> = measurements
        .iter()
        .map(|(pair, windows)| {
            windows
                .iter()
                .map(|window| {
                    let mut candidate = candidates
                        .iter()
                        .find(|c| (c.cpu_pwm, c.gpu_pwm) == *pair)
                        .context("candidate summary missing")?
                        .clone();
                    let mean = |get: fn(&Sample) -> f64| {
                        window.iter().map(get).sum::<f64>() / window.len() as f64
                    };
                    candidate.noise_dbfs = equivalent_level(
                        &window
                            .iter()
                            .map(|s| s.acoustic.window.as_ref().unwrap().level_dbfs)
                            .collect::<Vec<_>>(),
                    )?;
                    candidate.cpu_temp_c = mean(|s| s.thermal.cpu_c);
                    candidate.cpu_rpm = Some(mean(|s| f64::from(s.cpu_rpm)).round() as u32);
                    candidate.gpu_rpm = Some(mean(|s| f64::from(s.gpu_rpm)).round() as u32);
                    candidate.gpu_temp_c = window
                        .iter()
                        .all(|s| s.thermal.gpu_c.is_some())
                        .then(|| mean(|s| s.thermal.gpu_c.unwrap()));
                    candidate.power_w = mean(|s| s.thermal.cpu_w + s.thermal.gpu_w.unwrap_or(0.0));
                    candidate.cpu_power_w = Some(mean(|s| s.thermal.cpu_w));
                    candidate.gpu_power_w = window
                        .iter()
                        .all(|s| s.thermal.gpu_w.is_some())
                        .then(|| mean(|s| s.thermal.gpu_w.unwrap()));
                    candidate.repetitions = 1;
                    Ok(candidate)
                })
                .collect::<Result<_>>()
        })
        .collect::<Result<_>>()?;
    let selected = select_ceiling(&candidates, &policy).and_then(|selected| {
        let quiet_index = measurements
            .iter()
            .position(|(p, _)| *p == (selected.cpu_pwm, selected.gpu_pwm))
            .context("selected trial missing")?;
        let loud_index = quiet_index + 1;
        ensure!(
            loud_index < repeated.len(),
            "no louder trial paired with selected ceiling"
        );
        verify_repeated_knee(&repeated[quiet_index], &repeated[loud_index], &policy)?;
        Ok(selected)
    });
    let success =
        result.is_ok() && workload_restore.is_ok() && curve_restore.is_ok() && heat_restore.is_ok();
    let report = json!({"schema_version":1, "profile":runtime.active.name(), "context":options.context,
        "hardware":super::quiet::hardware_identity()?, "baseline":baseline, "original_curves":session.curves.original,
        "workload":options.workload, "gpu":options.gpu, "require_gpu_load":options.require_gpu_load,
        "cpu_ppt":cpu_ppt, "gpu_clock_mhz":options.gpu_clock_mhz, "power_restored":heat_restore.is_ok(),
        "selection_policy":policy, "candidates":candidates, "selected":if success { selected.as_ref().ok() } else { None },
        "selection_error":selected.err().map(|e|e.to_string()), "trials":measurements, "samples":session.all_samples,
        "repeated_candidates":repeated,
        "benchmark_error":result.as_ref().err().map(|e|format!("{e:#}")), "workload_stopped":workload_restore.is_ok(), "amc_yields":session.yields,
        "curves_restored":curve_restore.is_ok(), "qualification":"candidate for the tested workload/profile/setup; requires burst, combined-load and lifecycle acceptance"});
    // Atomic metrics-only report: an interrupted write cannot masquerade as a
    // completed previous qualification or destroy its evidence in place.
    let parent = options
        .report
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&serde_json::to_vec_pretty(&report)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(&options.report)?;
    std::fs::File::open(parent)?.sync_all()?;
    workload_restore?;
    curve_restore?;
    heat_restore?;
    result?;
    ensure!(
        !report["selected"].is_null(),
        "benchmark completed but acoustic selection was inconclusive; retained report {}",
        options.report.display()
    );
    println!(
        "{}",
        json!({"type":"summary", "selected":report["selected"], "report":options.report})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calibration_rejects_full_speed_reversed_and_single_point_sweeps() {
        assert!(levels("40:40,100:100").is_ok());
        for value in ["40:40,255:255", "40:40,100:20", "40:40", "0:40,100:100"] {
            assert!(levels(value).is_err());
        }
    }
    #[test]
    fn amc_contenders_are_capacity_waiters_and_stay_live_until_finished() {
        let status = br#"{"budget_bytes":1,"reserve_bytes":1,"committed_bytes":0,
            "entries":[{"id":"a","phase":"queued"},{"id":"b","phase":"queued"},{"id":"c","phase":"running"}],
            "waiting":{"a":{"reason":"shared_headroom","headroom":null},"b":{"reason":"paused","headroom":null},
                "c":{"reason":"fifo","headroom":null}},
            "unreconciled":[]}"#;
        let (waiting, live) = contenders(status).unwrap();
        assert_eq!(waiting, BTreeSet::from(["a".to_owned()]));
        assert_eq!(live.len(), 3);
        assert!(contenders(b"{}").is_err());
    }
}
