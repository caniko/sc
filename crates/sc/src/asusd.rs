use anyhow::{Context, Result};
use clap::Subcommand;
use sc_core::asusd_config::{self, Fan, Profile, WireCurve};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use zbus::blocking::Connection;

#[derive(Subcommand)]
pub enum Command {
    /// Show supported platform profiles and firmware fan curves
    Status,
    /// Preview or transactionally apply a Pkl fan-curve configuration
    Apply {
        /// Path to the asusd Pkl configuration
        #[arg(long)]
        config: PathBuf,
        /// Perform writes; without this flag the command only previews
        #[arg(long)]
        apply: bool,
    },
    /// Preview or reset one profile to its firmware defaults
    Reset {
        /// Platform profile to reset
        #[arg(long)]
        profile: String,
        /// Perform the reset; without this flag the command only previews
        #[arg(long)]
        apply: bool,
    },
}

#[zbus::proxy(
    interface = "xyz.ljones.Platform",
    default_service = "xyz.ljones.Asusd",
    default_path = "/xyz/ljones",
    gen_async = false
)]
trait PlatformBus {
    #[zbus(property)]
    fn platform_profile_choices(&self) -> zbus::Result<Vec<u32>>;

    #[zbus(property)]
    fn platform_profile(&self) -> zbus::Result<u32>;

    #[zbus(property)]
    fn set_platform_profile(&self, profile: u32) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "xyz.ljones.FanCurves",
    default_service = "xyz.ljones.Asusd",
    default_path = "/xyz/ljones",
    gen_async = false
)]
trait FanCurvesBus {
    fn fan_curve_data(&self, profile: u32) -> zbus::Result<Vec<WireCurve>>;
    fn set_fan_curve(&self, profile: u32, curve: WireCurve) -> zbus::Result<()>;
    fn set_curves_to_defaults(&self, profile: u32) -> zbus::Result<()>;
}

struct RuntimeState {
    active: Profile,
    profiles: Vec<Profile>,
    curves: HashMap<Profile, Vec<WireCurve>>,
}

struct Change {
    profile: Profile,
    desired: WireCurve,
    snapshot: WireCurve,
}

pub fn run(command: Command) -> Result<()> {
    let connection = Connection::system().context("connect to the system D-Bus")?;
    let platform = PlatformBusProxy::builder(&connection)
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .context("connect to asusd Platform")?;
    let fans = FanCurvesBusProxy::new(&connection).context("connect to asusd FanCurves")?;

    match command {
        Command::Status => status(&platform, &fans),
        Command::Apply { config, apply } => apply_config(&platform, &fans, &config, apply),
        Command::Reset { profile, apply } => reset(&platform, &fans, &profile, apply),
    }
}

fn status(platform: &PlatformBusProxy<'_>, fans: &FanCurvesBusProxy<'_>) -> Result<()> {
    let runtime = discover(platform, fans)?;
    println!("Active platform profile: {}", runtime.active.name());
    println!("Supported profiles and fan curves:");
    for profile in &runtime.profiles {
        println!("  {}:", profile.name());
        for curve in &runtime.curves[profile] {
            print_curve("    ", curve);
        }
    }
    Ok(())
}

fn apply_config(
    platform: &PlatformBusProxy<'_>,
    fans: &FanCurvesBusProxy<'_>,
    path: &Path,
    apply: bool,
) -> Result<()> {
    let config = asusd_config::load(path)?;
    let runtime = discover(platform, fans)?;
    let mut changes = Vec::new();

    for configured_profile in &config.profiles {
        let profile = Profile::parse(&configured_profile.name)?;
        anyhow::ensure!(
            runtime.profiles.contains(&profile),
            "profile '{}' is not supported by this platform",
            profile.name()
        );
        let available = &runtime.curves[&profile];
        for curve in &configured_profile.curves {
            let fan = Fan::parse(&curve.fan)?;
            let snapshot = available
                .iter()
                .find(|candidate| Fan::parse(&candidate.0).ok() == Some(fan))
                .cloned()
                .with_context(|| {
                    format!(
                        "fan '{}' is not supported for profile '{}'",
                        fan.name(),
                        profile.name()
                    )
                })?;
            changes.push(Change {
                profile,
                desired: curve.wire()?,
                snapshot,
            });
        }
    }

    println!("Planned asusd fan-curve changes:");
    for change in &changes {
        print!("  {} ", change.profile.name());
        print_curve("", &change.desired);
    }
    if !apply {
        println!("Preview only; pass --apply to write these curves.");
        return Ok(());
    }

    transact(
        &changes,
        |change| {
            fans.set_fan_curve(change.profile.id(), change.desired.clone())
                .with_context(|| {
                    format!(
                        "set {}/{} fan curve",
                        change.profile.name(),
                        change.desired.0
                    )
                })
        },
        |change| {
            let curves = read_curves(fans, change.profile)?;
            anyhow::ensure!(
                curves.contains(&change.desired),
                "read-back mismatch for {}/{}",
                change.profile.name(),
                change.desired.0
            );
            preserve_active_profile(platform, runtime.active)
        },
        |change| {
            let mut errors = Vec::new();
            match fans
                .set_fan_curve(change.profile.id(), change.snapshot.clone())
                .with_context(|| {
                    format!(
                        "restore {}/{} fan curve",
                        change.profile.name(),
                        change.snapshot.0
                    )
                }) {
                Ok(()) => match read_curves(fans, change.profile) {
                    Ok(curves) if curves.contains(&change.snapshot) => {}
                    Ok(_) => errors.push(format!(
                        "rollback read-back mismatch for {}/{}",
                        change.profile.name(),
                        change.snapshot.0
                    )),
                    Err(error) => errors.push(error.to_string()),
                },
                Err(error) => errors.push(error.to_string()),
            }
            if let Err(error) = restore_active_profile(platform, runtime.active) {
                errors.push(error.to_string());
            }
            anyhow::ensure!(errors.is_empty(), "{}", errors.join("; "));
            Ok(())
        },
    )?;

    println!("Applied and verified {} fan curve(s).", changes.len());
    Ok(())
}

fn reset(
    platform: &PlatformBusProxy<'_>,
    fans: &FanCurvesBusProxy<'_>,
    requested_profile: &str,
    apply: bool,
) -> Result<()> {
    let profile = Profile::parse(requested_profile)?;
    let runtime = discover(platform, fans)?;
    anyhow::ensure!(
        runtime.profiles.contains(&profile),
        "profile '{}' is not supported by this platform",
        profile.name()
    );
    let snapshots = runtime.curves[&profile].clone();
    anyhow::ensure!(
        !snapshots.is_empty(),
        "profile '{}' has no readable fan curves",
        profile.name()
    );

    println!(
        "Would reset {} curve(s) for '{}' to firmware defaults.",
        snapshots.len(),
        profile.name()
    );
    if !apply {
        println!("Preview only; pass --apply to reset the curves.");
        return Ok(());
    }

    transact(
        &[()],
        |_| {
            fans.set_curves_to_defaults(profile.id())
                .with_context(|| format!("reset '{}' curves", profile.name()))
        },
        |_| {
            let curves = read_curves(fans, profile)?;
            let expected_fans = snapshots
                .iter()
                .map(|curve| Fan::parse(&curve.0))
                .collect::<Result<HashSet<_>>>()?;
            let actual_fans = curves
                .iter()
                .map(|curve| Fan::parse(&curve.0))
                .collect::<Result<HashSet<_>>>()?;
            anyhow::ensure!(
                curves.len() == snapshots.len() && actual_fans == expected_fans,
                "reset read-back fan set mismatch for '{}'",
                profile.name()
            );
            preserve_active_profile(platform, runtime.active)
        },
        |_| {
            let mut errors = Vec::new();
            for snapshot in snapshots.iter().rev() {
                if let Err(error) = fans.set_fan_curve(profile.id(), snapshot.clone()) {
                    errors.push(format!(
                        "restore {}/{} fan curve: {error}",
                        profile.name(),
                        snapshot.0
                    ));
                }
            }
            match read_curves(fans, profile) {
                Ok(curves) => {
                    for snapshot in &snapshots {
                        if !curves.contains(snapshot) {
                            errors.push(format!(
                                "rollback read-back mismatch for {}/{}",
                                profile.name(),
                                snapshot.0
                            ));
                        }
                    }
                }
                Err(error) => errors.push(error.to_string()),
            }
            if let Err(error) = restore_active_profile(platform, runtime.active) {
                errors.push(error.to_string());
            }
            anyhow::ensure!(errors.is_empty(), "{}", errors.join("; "));
            Ok(())
        },
    )?;

    println!("Reset and verified '{}' firmware curves.", profile.name());
    Ok(())
}

fn discover(platform: &PlatformBusProxy<'_>, fans: &FanCurvesBusProxy<'_>) -> Result<RuntimeState> {
    let profiles = platform
        .platform_profile_choices()
        .context("read supported asusd platform profiles")?
        .into_iter()
        .map(Profile::from_id)
        .collect::<Result<Vec<_>>>()?;
    let active = Profile::from_id(
        platform
            .platform_profile()
            .context("read active asusd platform profile")?,
    )?;
    let curves = profiles
        .iter()
        .copied()
        .map(|profile| Ok((profile, read_curves(fans, profile)?)))
        .collect::<Result<HashMap<_, _>>>()?;
    Ok(RuntimeState {
        active,
        profiles,
        curves,
    })
}

fn read_curves(fans: &FanCurvesBusProxy<'_>, profile: Profile) -> Result<Vec<WireCurve>> {
    fans.fan_curve_data(profile.id())
        .with_context(|| format!("read '{}' fan curves", profile.name()))
}

fn preserve_active_profile(platform: &PlatformBusProxy<'_>, expected: Profile) -> Result<()> {
    let current = Profile::from_id(
        platform
            .platform_profile()
            .context("verify active asusd platform profile")?,
    )?;
    if current != expected {
        restore_active_profile(platform, expected)?;
        anyhow::bail!(
            "active platform profile changed from '{}' to '{}'",
            expected.name(),
            current.name()
        );
    }
    Ok(())
}

fn restore_active_profile(platform: &PlatformBusProxy<'_>, expected: Profile) -> Result<()> {
    if Profile::from_id(platform.platform_profile()?)? != expected {
        platform
            .set_platform_profile(expected.id())
            .with_context(|| format!("restore active platform profile '{}'", expected.name()))?;
        let restored = Profile::from_id(
            platform
                .platform_profile()
                .context("verify restored asusd platform profile")?,
        )?;
        anyhow::ensure!(
            restored == expected,
            "active platform profile restoration read-back mismatch: expected '{}', got '{}'",
            expected.name(),
            restored.name()
        );
    }
    Ok(())
}

fn transact<T>(
    items: &[T],
    mut write: impl FnMut(&T) -> Result<()>,
    mut verify: impl FnMut(&T) -> Result<()>,
    mut rollback: impl FnMut(&T) -> Result<()>,
) -> Result<()> {
    for (index, item) in items.iter().enumerate() {
        if let Err(error) = write(item).and_then(|_| verify(item)) {
            let rollback_errors = items[..=index]
                .iter()
                .rev()
                .filter_map(|attempted| rollback(attempted).err().map(|error| error.to_string()))
                .collect::<Vec<_>>();
            if rollback_errors.is_empty() {
                return Err(error.context("asusd transaction rolled back"));
            }
            return Err(error.context(format!(
                "asusd transaction rollback also failed: {}",
                rollback_errors.join("; ")
            )));
        }
    }
    Ok(())
}

fn print_curve(prefix: &str, curve: &WireCurve) {
    println!(
        "{prefix}{} enabled={} temp={:?} pwm={:?}",
        curve.0, curve.3, curve.2, curve.1
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use zbus::zvariant::Type;

    #[test]
    fn curve_wire_type_matches_asusd() {
        assert_eq!(WireCurve::SIGNATURE.to_string(), "(s(yyyyyyyy)(yyyyyyyy)b)");
    }

    #[test]
    fn transaction_rolls_back_attempted_items_in_reverse() {
        let events = RefCell::new(Vec::new());
        let error = transact(
            &[1, 2, 3],
            |item| {
                events.borrow_mut().push(format!("write {item}"));
                Ok(())
            },
            |item| {
                events.borrow_mut().push(format!("verify {item}"));
                anyhow::ensure!(*item != 2, "verification failed");
                Ok(())
            },
            |item| {
                events.borrow_mut().push(format!("rollback {item}"));
                Ok(())
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("rolled back"));
        assert_eq!(
            events.into_inner(),
            [
                "write 1",
                "verify 1",
                "write 2",
                "verify 2",
                "rollback 2",
                "rollback 1",
            ]
        );
    }

    #[test]
    fn transaction_reports_rollback_verification_failure() {
        let error = transact(
            &[1],
            |_| Ok(()),
            |_| anyhow::bail!("apply verification failed"),
            |_| anyhow::bail!("rollback read-back mismatch"),
        )
        .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("apply verification failed"));
        assert!(message.contains("rollback read-back mismatch"));
    }
}
