use super::benchmark::{cpu_temperature, gpu_active, gpu_temperature, same_curves};
use super::{read_curves, FanCurvesBusProxy, PlatformBusProxy, Profile, WireCurve};
use crate::acoustic::interrupt;
use crate::process::output;
use anyhow::{ensure, Context, Result};
use sc_core::quiet::{Controller, Mode, ThermalInput};
use sc_hwmon::telemetry::read_snapshot;
use serde_json::json;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub fn hardware_identity() -> Result<sc_core::quiet::HardwareIdentity> {
    let read = |name| -> Result<String> {
        let value = std::fs::read_to_string(Path::new("/sys/class/dmi/id").join(name))?;
        ensure!(!value.trim().is_empty(), "DMI {name} unavailable");
        Ok(value.trim().into())
    };
    let mut gpu_devices = Vec::new();
    for device in std::fs::read_dir("/sys/bus/pci/devices")? {
        let device = device?;
        let vendor = std::fs::read_to_string(device.path().join("vendor"))?;
        let class = std::fs::read_to_string(device.path().join("class"))?;
        if vendor.trim() == "0x10de" && class.trim().starts_with("0x03") {
            gpu_devices.push(format!(
                "{}:{}",
                device.file_name().to_string_lossy(),
                std::fs::read_to_string(device.path().join("device"))?.trim()
            ));
        }
    }
    gpu_devices.sort();
    Ok(sc_core::quiet::HardwareIdentity {
        product: read("product_name")?,
        bios: read("bios_version")?,
        board: read("board_name")?,
        kernel: std::fs::read_to_string("/proc/sys/kernel/osrelease")?
            .trim()
            .into(),
        nvidia_driver: std::fs::read_to_string("/sys/module/nvidia/version")
            .unwrap_or_else(|_| "unloaded".into())
            .trim()
            .into(),
        gpu_devices,
    })
}

pub struct Lease(File);
impl Lease {
    pub fn acquire() -> Result<Self> {
        std::fs::create_dir_all("/run/smartcool")?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open("/run/smartcool/asusd-control.lock")?;
        // Persistent anchor: flock is released by the descriptor, never unlink.
        ensure!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0, "ASUS control is leased by another SmartCool operation; stop smartcool-asusd-quiet before benchmarking/applying curves");
        Ok(Self(file))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.0.as_raw_fd();
    }
}

#[zbus::proxy(
    interface = "xyz.ljones.AsusArmoury",
    default_service = "xyz.ljones.Asusd",
    gen_async = false
)]
trait Armoury {
    #[zbus(property)]
    fn current_value(&self) -> zbus::Result<i32>;
    #[zbus(property)]
    fn set_current_value(&self, value: i32) -> zbus::Result<()>;
    #[zbus(property)]
    fn min_value(&self) -> zbus::Result<i32>;
    #[zbus(property)]
    fn max_value(&self) -> zbus::Result<i32>;
}

fn ppt(values: [i32; 3]) -> Result<[i32; 3]> {
    let connection = zbus::blocking::connection::Builder::system()?
        .method_timeout(Duration::from_secs(1))
        .build()?;
    let mut proxies = Vec::new();
    for (name, value) in ["ppt_pl1_spl", "ppt_pl2_sppt", "ppt_pl3_fppt"]
        .into_iter()
        .zip(values)
    {
        let proxy = ArmouryProxy::builder(&connection)
            .path(format!("/xyz/ljones/asus_armoury/{name}"))?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()?;
        ensure!(
            (proxy.min_value()?..=proxy.max_value()?).contains(&value),
            "requested {name} exceeds device capability"
        );
        proxies.push(proxy);
    }
    // Validate all capabilities before the first write.
    for (proxy, value) in proxies.iter().zip(values) {
        if proxy.current_value()? != value {
            proxy.set_current_value(value)?;
            ensure!(proxy.current_value()? == value, "PPT readback mismatch");
        }
    }
    Ok(values)
}

fn minimum_ppt() -> Result<[i32; 3]> {
    let connection = zbus::blocking::connection::Builder::system()?
        .method_timeout(Duration::from_secs(1))
        .build()?;
    let mut values = [0; 3];
    for (name, value) in ["ppt_pl1_spl", "ppt_pl2_sppt", "ppt_pl3_fppt"]
        .into_iter()
        .zip(&mut values)
    {
        *value = ArmouryProxy::builder(&connection)
            .path(format!("/xyz/ljones/asus_armoury/{name}"))?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()?
            .min_value()?;
    }
    Ok(values)
}

fn read_ppt() -> Result<[i32; 3]> {
    let connection = zbus::blocking::connection::Builder::system()?
        .method_timeout(Duration::from_secs(1))
        .build()?;
    let mut values = [0; 3];
    for (name, value) in ["ppt_pl1_spl", "ppt_pl2_sppt", "ppt_pl3_fppt"]
        .into_iter()
        .zip(&mut values)
    {
        *value = ArmouryProxy::builder(&connection)
            .path(format!("/xyz/ljones/asus_armoury/{name}"))?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()?
            .current_value()?;
    }
    Ok(values)
}

pub struct HeatTrial {
    original: Option<[i32; 3]>,
    requested: Option<[i32; 3]>,
    gpu: Option<String>,
    restored: bool,
}

impl HeatTrial {
    pub fn begin(cpu: Option<[i32; 3]>, gpu: Option<&str>, clock: Option<u32>) -> Result<Self> {
        let mut trial = Self {
            original: cpu.map(|_| read_ppt()).transpose()?,
            requested: cpu,
            gpu: None,
            restored: false,
        };
        if let Some(cpu) = cpu {
            let actual = ppt(cpu)?;
            trial.requested = Some(actual);
            ensure!(
                actual == cpu,
                "trial CPU power request outside device capability"
            );
        }
        if let Some(clock) = clock {
            let gpu = gpu.context("GPU PCI BDF required")?;
            // Record the attempted change before the call: an uncertain result
            // still requires resetting the trial-owned request during cleanup.
            trial.gpu = Some(gpu.into());
            gpu_clock(gpu, Some(clock))?;
        }
        Ok(trial)
    }

    pub fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        let mut errors = Vec::new();
        if let Some(gpu) = &self.gpu {
            if let Err(e) = gpu_clock(gpu, None) {
                errors.push(format!("GPU clock reset: {e:#}"));
            }
        }
        if let Some(original) = self.original {
            match read_ppt() {
                Ok(actual)
                    if self.requested.is_some_and(|requested| {
                        actual.iter().zip(requested.iter().zip(original)).all(
                            |(actual, (requested, original))| {
                                *actual == *requested || *actual == original
                            },
                        )
                    }) =>
                {
                    if let Err(e) = ppt(original) {
                        errors.push(format!("PPT restore: {e:#}"));
                    }
                }
                Ok(_) => errors
                    .push("PPT changed by another writer; its newer values left in place".into()),
                Err(e) => errors.push(e.to_string()),
            }
        }
        ensure!(
            errors.is_empty(),
            "heat-control restoration failed: {}",
            errors.join("; ")
        );
        self.restored = true;
        Ok(())
    }
}

impl Drop for HeatTrial {
    fn drop(&mut self) {
        if let Err(e) = self.restore() {
            eprintln!("ASUS power recovery required: {e:#}");
        }
    }
}

fn gpu_clock(id: &str, maximum: Option<u32>) -> Result<()> {
    let mut command = Command::new("nvidia-smi");
    command.args(["-i", id]);
    if let Some(maximum) = maximum {
        command.arg(format!("--lock-gpu-clocks=210,{maximum}"));
    } else {
        command.arg("--reset-gpu-clocks");
    }
    output(&mut command, Duration::from_secs(1))?;
    Ok(())
}

fn fan_policy(
    curves: &[WireCurve],
    envelope: &sc_core::quiet::Envelope,
    config: &sc_core::quiet::Config,
) -> Result<()> {
    ensure!(
        curves.len() == 2,
        "quiet policy requires exactly CPU/GPU curves"
    );
    ensure!(
        curves.iter().any(|c| c.0 == "CPU") && curves.iter().any(|c| c.0 == "GPU"),
        "CPU/GPU fan set mismatch"
    );
    for curve in curves {
        let (cap, band, emergency) = match curve.0.as_str() {
            "CPU" => (
                envelope.cpu_pwm_ceiling,
                config.cpu_emergency_band_c,
                config.cpu_emergency_c,
            ),
            "GPU" => (
                envelope.gpu_pwm_ceiling,
                config.gpu_emergency_band_c,
                config.gpu_emergency_c,
            ),
            _ => anyhow::bail!("unsupported quiet fan"),
        };
        ensure!(
            curve.3 && curve.1[7] == 255 && f64::from(curve.2[7]) == emergency,
            "missing exact firmware emergency endpoint"
        );
        ensure!(
            f64::from(curve.2[6]) >= emergency - band && curve.1[..7].iter().all(|p| *p <= cap),
            "curve exceeds normal acoustic envelope before the emergency band"
        );
    }
    Ok(())
}

pub fn run(
    platform: &PlatformBusProxy<'_>,
    fans: &FanCurvesBusProxy<'_>,
    path: &Path,
) -> Result<()> {
    let config = sc_core::quiet::load(path)?;
    ensure!(
        config.hardware == hardware_identity()?,
        "quiet envelope invalid after device/BIOS change; requalify"
    );
    let _lease = Lease::acquire()?;
    let flag = interrupt()?;
    let runtime = super::discover(platform, fans)?;
    let minimum_power = minimum_ppt()?;
    for profile in &config.profiles {
        let id = Profile::parse(&profile.name)?;
        let curves = runtime
            .curves
            .get(&id)
            .context("configured quiet profile unavailable")?;
        fan_policy(curves, profile, &config)?;
    }
    let mut controller = Controller::default();
    let start = Instant::now();
    let mut previous: Option<(f64, f64, Option<f64>)> = None;
    let mut last_profile = runtime.active;
    let mut clock = None;
    let mut clock_owned = false;
    let mut last_mode = Mode::Normal;
    let mut expected = runtime.curves.clone();
    let mut stopped_since = std::collections::HashMap::<String, Instant>::new();
    sd_notify::notify(&[sd_notify::NotifyState::Ready])?;
    let result = (|| -> Result<()> {
        loop {
            std::thread::sleep(Duration::from_secs(1));
            if flag.load(Ordering::Relaxed) {
                break;
            }
            let profile = Profile::from_id(platform.platform_profile()?)?;
            let envelope = config
                .profiles
                .iter()
                .find(|e| e.name == profile.name())
                .context("active profile has no quiet envelope")?;
            ensure!(
                same_curves(&read_curves(fans, profile)?, &expected[&profile]),
                "another writer changed curves; quiet supervision stopped"
            );
            if profile != last_profile {
                previous = None;
                last_profile = profile;
            }
            let state = gpu_active(&config.gpu_pci_id);
            let active = *state.as_ref().unwrap_or(&true);
            let reading = state.and_then(|active| {
                let snapshot = read_snapshot(Path::new("/sys/class/hwmon"))?;
                let cpu = cpu_temperature(&snapshot)?;
                let gpu = if active {
                    Some(gpu_temperature(&config.gpu_pci_id)?)
                } else {
                    None
                };
                let fan_state = super::RuntimeState {
                    active: profile,
                    profiles: runtime.profiles.clone(),
                    curves: expected.clone(),
                };
                let rpms = super::verify::fan_rpms(&fan_state, &snapshot)?;
                for curve in &expected[&profile] {
                    let temp = if curve.0 == "CPU" {
                        cpu
                    } else if let Some(gpu) = gpu {
                        gpu
                    } else {
                        snapshot
                            .temperatures
                            .iter()
                            .find(|s| s.chip == "amdgpu" && s.label.as_deref() == Some("edge"))
                            .context("GPU-side firmware temperature unavailable")?
                            .temp_c
                            .context("GPU-side firmware temperature unreadable")?
                    };
                    let index = if curve.0 == "CPU" { 1 } else { 2 };
                    let driver = snapshot
                        .curves
                        .iter()
                        .find(|c| c.index == index)
                        .context("active firmware table unavailable")?;
                    ensure!(
                        driver.enabled == Some(true)
                            && driver.pwm == Some(curve.1)
                            && driver.temp == Some(curve.2),
                        "active driver curve differs from quiet-policy request"
                    );
                    let floor = if curve.0 == "CPU" {
                        config.cpu_start_pwm
                    } else {
                        config.gpu_start_pwm
                    };
                    let requested = requested_pwm(curve, temp);
                    let rpm = rpms.get(&curve.0).context("required fan RPM unavailable")?;
                    if requested >= f64::from(floor) && *rpm == 0 {
                        let since = stopped_since
                            .entry(curve.0.clone())
                            .or_insert_with(Instant::now);
                        ensure!(
                            since.elapsed() < Duration::from_secs(10),
                            "{} did not start at qualified running PWM",
                            curve.0
                        );
                    } else {
                        stopped_since.remove(&curve.0);
                    }
                }
                Ok((cpu, gpu))
            });
            let now = start.elapsed().as_secs_f64();
            let (cpu, gpu, cpu_rate, gpu_rate) = if let Ok((cpu, gpu)) = &reading {
                if previous.is_some_and(|(time, _, _)| now - time > 5.0) {
                    clock = None;
                    previous = None;
                }
                let rates = previous
                    .map(|(time, old_cpu, old_gpu)| {
                        (
                            (*cpu - old_cpu) / (now - time),
                            gpu.zip(old_gpu)
                                .map_or(0.0, |(a, b)| (a - b) / (now - time)),
                        )
                    })
                    .unwrap_or((0.0, 0.0));
                previous = Some((now, *cpu, *gpu));
                (Some(*cpu), *gpu, rates.0, rates.1)
            } else {
                previous = None;
                (None, None, 0.0, 0.0)
            };
            let mode = controller.update(
                &config,
                ThermalInput {
                    now,
                    cpu,
                    gpu,
                    gpu_required: active,
                    cpu_rate,
                    gpu_rate,
                },
            );
            let power = if mode == Mode::Normal {
                envelope.cpu_ppt
            } else {
                minimum_power
            };
            let full_fans = matches!(mode, Mode::Emergency | Mode::TelemetryFault);
            // Emergency is host-wide, not reset by GameMode/profile changes.
            for (id, original) in &runtime.curves {
                let desired: Vec<_> = original
                    .iter()
                    .cloned()
                    .map(|mut c| {
                        if full_fans {
                            c.1 = [255; 8];
                        }
                        c
                    })
                    .collect();
                if !same_curves(&desired, &expected[id]) {
                    ensure!(
                        same_curves(&read_curves(fans, *id)?, &expected[id]),
                        "another writer changed fan curves"
                    );
                    for curve in &desired {
                        let table = expected.get_mut(id).context("profile state missing")?;
                        table.retain(|c| c.0 != curve.0);
                        table.push(curve.clone());
                        fans.set_fan_curve(id.id(), curve.clone())?;
                    }
                    ensure!(
                        same_curves(&read_curves(fans, *id)?, &desired),
                        "fan request readback mismatch"
                    );
                }
            }
            let power = ppt(power)?;
            if active {
                let maximum = if mode == Mode::Normal {
                    envelope.gpu_clock_mhz
                } else {
                    config.gpu_reduced_clock_mhz
                };
                if clock != Some(maximum) {
                    clock_owned = true;
                    gpu_clock(&config.gpu_pci_id, Some(maximum))?;
                    clock = Some(maximum);
                }
            } else {
                clock = None;
            }
            if mode != last_mode || reading.is_err() {
                writeln!(
                    std::io::stdout(),
                    "{}",
                    json!({"type":"quiet_policy", "profile":profile.name(), "mode":mode, "cpu_c":cpu, "gpu_c":gpu, "gpu_active":active, "cpu_ppt":power, "gpu_clock_mhz":clock, "thermal_error":reading.err().map(|e|format!("{e:#}"))})
                )?;
                last_mode = mode;
            }
            sd_notify::notify(&[sd_notify::NotifyState::Watchdog])?;
        }
        Ok(())
    })();
    // Restore only still-owned fan tables; a newer external policy wins.
    let mut restoration = Vec::new();
    for (profile, original) in &runtime.curves {
        match read_curves(fans, *profile) {
            Ok(current) => {
                for curve in original {
                    let owned = expected[profile].iter().find(|c| c.0 == curve.0);
                    let actual = current.iter().find(|c| c.0 == curve.0);
                    if actual != owned && actual != Some(curve) {
                        restoration.push(format!(
                            "newer {} {} curve left in place",
                            profile.name(),
                            curve.0
                        ));
                    } else if let Err(e) = fans.set_fan_curve(profile.id(), curve.clone()) {
                        restoration.push(e.to_string());
                    }
                }
            }
            Err(e) => restoration.push(e.to_string()),
        }
    }
    if clock_owned {
        if let Err(e) = gpu_clock(&config.gpu_pci_id, None) {
            restoration.push(e.to_string());
        }
    }
    // Keep a conservative reduced PPT on fault; normal exit restores the
    // active profile's declared envelope rather than another profile's snapshot.
    if result.is_ok() {
        if let Ok(profile) = Profile::from_id(platform.platform_profile()?) {
            if let Some(envelope) = config.profiles.iter().find(|e| e.name == profile.name()) {
                if let Err(e) = ppt(envelope.cpu_ppt) {
                    restoration.push(e.to_string());
                }
            }
        }
    }
    ensure!(
        restoration.is_empty(),
        "quiet-policy restoration: {}",
        restoration.join("; ")
    );
    result
}

fn requested_pwm(curve: &WireCurve, temp: f64) -> f64 {
    for i in 1..8 {
        if temp < f64::from(curve.2[i]) {
            if temp <= f64::from(curve.2[i - 1]) {
                return f64::from(curve.1[i - 1]);
            }
            let fraction =
                (temp - f64::from(curve.2[i - 1])) / f64::from(curve.2[i] - curve.2[i - 1]);
            return f64::from(curve.1[i - 1])
                + fraction * (f64::from(curve.1[i]) - f64::from(curve.1[i - 1]));
        }
    }
    f64::from(curve.1[7])
}
