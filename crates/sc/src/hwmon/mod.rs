pub mod fan;
pub mod sensor;

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Find the sysfs hwmon directory for a chip by name.
/// Scans /sys/class/hwmon/hwmon* and returns the path whose `name` file matches.
pub fn find_hwmon_path(chip_name: &str) -> Result<PathBuf> {
    let hwmon_base = Path::new("/sys/class/hwmon");

    for entry in fs::read_dir(hwmon_base).context("failed to read /sys/class/hwmon")? {
        let entry = entry?;
        let name_path = entry.path().join("name");

        if let Ok(name) = fs::read_to_string(&name_path) {
            if name.trim() == chip_name {
                return Ok(entry.path());
            }
        }
    }

    anyhow::bail!("hwmon chip '{}' not found in /sys/class/hwmon", chip_name)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_hwmon_returns_error_for_missing_chip() {
        let result = find_hwmon_path("nonexistent_chip_12345");
        assert!(result.is_err());
    }
}
