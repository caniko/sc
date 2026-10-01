use super::{Fan, Profile, RuntimeState};
use anyhow::{Context, Result};
use sc_core::asusd_config::Config;
use sc_hwmon::telemetry::Snapshot;
use std::collections::BTreeMap;

pub(super) fn curve_mismatches(
    config: &Config,
    runtime: &RuntimeState,
    snapshot: &Snapshot,
) -> Vec<String> {
    let mut issues = Vec::new();
    for profile in &config.profiles {
        let checked = (|| -> Result<()> {
            let id = Profile::parse(&profile.name)?;
            let stored = runtime
                .curves
                .get(&id)
                .context("profile unavailable from asusd")?;
            for curve in &profile.curves {
                let desired = curve.wire()?;
                if !stored.contains(&desired) {
                    issues.push(format!(
                        "stored {}/{} differs from declaration",
                        id.name(),
                        desired.0
                    ));
                }
                if id != runtime.active {
                    continue;
                }
                let index = fan_index(Fan::parse(&desired.0)?);
                let live = snapshot
                    .curves
                    .iter()
                    .filter(|curve| curve.index == index)
                    .collect::<Vec<_>>();
                if live.len() != 1 {
                    issues.push(format!(
                        "active kernel {}/{} unavailable or ambiguous",
                        id.name(),
                        desired.0
                    ));
                } else if live[0].enabled != Some(desired.3)
                    || (desired.3
                        && (live[0].pwm != Some(desired.1) || live[0].temp != Some(desired.2)))
                {
                    issues.push(format!(
                        "active kernel {}/{} differs or is unreadable",
                        id.name(),
                        desired.0
                    ));
                }
            }
            Ok(())
        })();
        if let Err(error) = checked {
            issues.push(format!("{}: {error:#}", profile.name));
        }
    }
    if !config
        .profiles
        .iter()
        .any(|profile| Profile::parse(&profile.name).ok() == Some(runtime.active))
    {
        issues.push(format!(
            "active profile '{}' has no declaration to verify",
            runtime.active.name()
        ));
    }
    issues
}

pub(super) fn fan_rpms(
    runtime: &RuntimeState,
    snapshot: &Snapshot,
) -> Result<BTreeMap<String, u32>> {
    let expected = runtime
        .curves
        .get(&runtime.active)
        .context("active fan set unavailable")?;
    anyhow::ensure!(
        !expected.is_empty(),
        "active profile has no declared ASUS fans"
    );
    let mut rpms = BTreeMap::new();
    for curve in expected {
        let fan = Fan::parse(&curve.0)?;
        let readings = snapshot
            .fans
            .iter()
            .filter(|reading| {
                reading.chip == "asus"
                    && match reading.label.as_deref() {
                        Some(label) => label == fan_label(fan),
                        None => reading.index == fan_index(fan),
                    }
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            readings.len() == 1,
            "{} RPM unavailable or ambiguous",
            fan.name()
        );
        let rpm = readings[0].rpm.with_context(|| {
            format!(
                "{} RPM unreadable: {}",
                fan.name(),
                readings[0].error.as_deref().unwrap_or("unknown")
            )
        })?;
        anyhow::ensure!(
            rpms.insert(fan.name().to_owned(), rpm).is_none(),
            "duplicate ASUS fan {}",
            fan.name()
        );
    }
    Ok(rpms)
}

fn fan_index(fan: Fan) -> u32 {
    match fan {
        Fan::Cpu => 1,
        Fan::Gpu => 2,
        Fan::Mid => 3,
    }
}

fn fan_label(fan: Fan) -> &'static str {
    match fan {
        Fan::Cpu => "cpu_fan",
        Fan::Gpu => "gpu_fan",
        Fan::Mid => "mid_fan",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sc_core::asusd_config::{Config, CurveConfig, ProfileConfig};
    use sc_hwmon::telemetry::{FanReading, FirmwareCurve, Snapshot};
    use std::collections::HashMap;

    fn fixture() -> (RuntimeState, Config, Snapshot) {
        let curve = CurveConfig {
            fan: "CPU".into(),
            temp: vec![40, 50, 55, 60, 65, 75, 85, 90],
            pwm: vec![0, 0, 0, 40, 70, 120, 200, 255],
            enabled: true,
        };
        let wire = curve.wire().unwrap();
        let runtime = RuntimeState {
            active: Profile::Quiet,
            profiles: vec![Profile::Quiet],
            curves: HashMap::from([(Profile::Quiet, vec![wire.clone()])]),
        };
        let config = Config {
            profiles: vec![ProfileConfig {
                name: "quiet".into(),
                curves: vec![curve],
            }],
        };
        let snapshot = Snapshot {
            fans: vec![FanReading {
                chip: "asus".into(),
                path: "/fake/hwmon37/fan1_input".into(),
                index: 1,
                label: Some("cpu_fan".into()),
                rpm: Some(0),
                error: None,
            }],
            curves: vec![FirmwareCurve {
                path: "/fake/hwmon8".into(),
                index: 1,
                temp: Some(wire.2),
                pwm: Some(wire.1),
                enabled: Some(true),
                error: None,
            }],
            ..Snapshot::default()
        };
        (runtime, config, snapshot)
    }

    #[test]
    fn kernel_mismatch_fails_even_when_stored_curve_matches() {
        let (runtime, config, mut snapshot) = fixture();
        assert!(curve_mismatches(&config, &runtime, &snapshot).is_empty());
        snapshot.curves[0].pwm.as_mut().unwrap()[2] = 10;
        assert!(curve_mismatches(&config, &runtime, &snapshot)
            .iter()
            .any(|issue| issue.contains("active kernel")));
    }

    #[test]
    fn missing_rpm_and_missing_fans_cannot_verify_silence() {
        let (runtime, _, mut snapshot) = fixture();
        assert_eq!(fan_rpms(&runtime, &snapshot).unwrap()["CPU"], 0);
        snapshot.fans[0].rpm = None;
        assert!(fan_rpms(&runtime, &snapshot).is_err());
        snapshot.fans.clear();
        assert!(fan_rpms(&runtime, &snapshot).is_err());
    }

    #[test]
    fn duplicate_fan_readings_are_ambiguous() {
        let (runtime, _, mut snapshot) = fixture();
        snapshot.fans.push(FanReading {
            chip: "asus".into(),
            path: "/fake/hwmon40/fan1_input".into(),
            index: 1,
            label: Some("cpu_fan".into()),
            rpm: Some(0),
            error: None,
        });
        assert!(fan_rpms(&runtime, &snapshot).is_err());
    }

    #[test]
    fn disabled_or_missing_kernel_curve_does_not_match_enabled_declaration() {
        let (runtime, config, mut snapshot) = fixture();
        snapshot.curves[0].enabled = Some(false);
        assert!(!curve_mismatches(&config, &runtime, &snapshot).is_empty());
        snapshot.curves.clear();
        assert!(!curve_mismatches(&config, &runtime, &snapshot).is_empty());
    }

    #[test]
    fn inactive_profile_is_verified_in_storage_without_switching_profiles() {
        let (mut runtime, mut config, snapshot) = fixture();
        runtime.profiles.push(Profile::Balanced);
        runtime
            .curves
            .insert(Profile::Balanced, runtime.curves[&Profile::Quiet].clone());
        let mut inactive = config.profiles[0].clone();
        inactive.name = "balanced".into();
        inactive.curves[0].pwm[3] = 50;
        config.profiles.push(inactive);
        let issues = curve_mismatches(&config, &runtime, &snapshot);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("stored balanced/CPU"));
    }

    #[test]
    fn unsupported_or_undeclared_active_profile_fails_verification() {
        let (runtime, mut config, snapshot) = fixture();
        config.profiles[0].name = "balanced".into();
        let issues = curve_mismatches(&config, &runtime, &snapshot);
        assert!(issues.iter().any(|issue| issue.contains("unavailable")));
        assert!(issues.iter().any(|issue| issue.contains("no declaration")));
    }
}
