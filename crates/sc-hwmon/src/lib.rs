pub mod fan;
pub mod sensor;

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Find all sysfs hwmon directories matching a chip name.
pub fn find_hwmon_paths(chip_name: &str) -> Result<Vec<PathBuf>> {
    let hwmon_base = Path::new("/sys/class/hwmon");
    let mut paths = Vec::new();

    for entry in fs::read_dir(hwmon_base).context("failed to read /sys/class/hwmon")? {
        let entry = entry?;
        let name_path = entry.path().join("name");

        if let Ok(name) = fs::read_to_string(&name_path) {
            if name.trim() == chip_name {
                paths.push(entry.path());
            }
        }
    }

    if paths.is_empty() {
        anyhow::bail!("hwmon chip '{}' not found in /sys/class/hwmon", chip_name);
    }
    Ok(paths)
}

/// Find the sysfs hwmon directory for a chip by name.
/// When multiple instances exist (e.g. two amdgpu), returns the first match.
pub fn find_hwmon_path(chip_name: &str) -> Result<PathBuf> {
    find_hwmon_paths(chip_name).map(|mut v| v.remove(0))
}

/// Read a sysfs file and parse its content as the given type.
pub fn read_sysfs<T: std::str::FromStr>(path: &Path) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    content
        .trim()
        .parse::<T>()
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {}", path.display(), e))
}

/// Write a value to a sysfs file.
pub fn write_sysfs(path: &Path, value: &str) -> Result<()> {
    fs::write(path, value)
        .with_context(|| format!("failed to write '{}' to {}", value, path.display()))
}
