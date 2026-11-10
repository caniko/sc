//! Relative microphone measurements and conservative, repeatable ceiling selection.
//! dBFS is a digital level, not calibrated sound pressure at the listener's ear.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoiseWindow {
    pub level_dbfs: f64,
    pub peak: f64,
    pub crest_factor: f64,
    pub samples: usize,
}

impl NoiseWindow {
    pub fn from_pcm(samples: &[f32]) -> Result<Self> {
        ensure!(samples.len() >= 4800, "insufficient microphone samples");
        ensure!(
            samples.iter().all(|x| x.is_finite()),
            "nonfinite microphone samples"
        );
        let count = samples.len() as f64;
        let mean = samples.iter().map(|x| f64::from(*x)).sum::<f64>() / count;
        let power = samples
            .iter()
            .map(|x| (f64::from(*x) - mean).powi(2))
            .sum::<f64>()
            / count;
        let peak = samples
            .iter()
            .map(|x| f64::from(*x).abs())
            .fold(0.0, f64::max);
        ensure!(peak < 0.99, "microphone clipping");
        ensure!(
            power > 1e-12,
            "microphone digital silence or constant signal"
        );
        let ac_peak = samples
            .iter()
            .map(|x| (f64::from(*x) - mean).abs())
            .fold(0.0, f64::max);
        let crest_factor = ac_peak / power.sqrt();
        ensure!(
            crest_factor <= 12.0,
            "transient-contaminated microphone window"
        );
        let block_levels: Vec<_> = samples
            .chunks(4800)
            .map(|block| {
                let mean = block.iter().map(|x| f64::from(*x)).sum::<f64>() / block.len() as f64;
                let power = block
                    .iter()
                    .map(|x| (f64::from(*x) - mean).powi(2))
                    .sum::<f64>()
                    / block.len() as f64;
                10.0 * power.max(1e-15).log10()
            })
            .collect();
        let spread = block_levels
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max)
            - block_levels.iter().copied().fold(f64::INFINITY, f64::min);
        ensure!(
            spread <= 6.0,
            "nonstationary microphone window (speech, movement or changing noise)"
        );
        Ok(Self {
            level_dbfs: 10.0 * power.log10(),
            peak,
            crest_factor,
            samples: samples.len(),
        })
    }
}

/// Joint CPU/GPU operating point. Measurements must have the same workload,
/// microphone, gain, profile and physical setup before they are compared.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub cpu_pwm: u8,
    pub gpu_pwm: u8,
    pub noise_dbfs: f64,
    pub cpu_temp_c: f64,
    pub gpu_temp_c: Option<f64>,
    pub cpu_rpm: Option<u32>,
    pub gpu_rpm: Option<u32>,
    pub power_w: f64,
    #[serde(default)]
    pub cpu_power_w: Option<f64>,
    #[serde(default)]
    pub gpu_power_w: Option<f64>,
    pub slope_c_per_min: f64,
    pub repetitions: u32,
    pub valid: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub minimum_noise_step_db: f64,
    pub maximum_marginal_cooling_c: f64,
    pub maximum_power_difference_fraction: f64,
    pub maximum_slope_c_per_min: f64,
    pub cpu_limit_c: f64,
    pub gpu_limit_c: f64,
    pub minimum_repetitions: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            minimum_noise_step_db: 3.0,
            maximum_marginal_cooling_c: 1.0,
            maximum_power_difference_fraction: 0.05,
            maximum_slope_c_per_min: 0.2,
            cpu_limit_c: 85.0,
            gpu_limit_c: 77.0,
            minimum_repetitions: 3,
        }
    }
}

impl Policy {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.minimum_repetitions >= 3,
            "at least three repetitions required"
        );
        ensure!(
            self.minimum_noise_step_db.is_finite() && self.minimum_noise_step_db >= 1.0,
            "invalid noise step"
        );
        ensure!(
            self.maximum_marginal_cooling_c.is_finite() && self.maximum_marginal_cooling_c > 0.0,
            "invalid cooling tolerance"
        );
        ensure!(
            self.maximum_power_difference_fraction.is_finite()
                && (0.0..=0.1).contains(&self.maximum_power_difference_fraction),
            "invalid matched-power tolerance"
        );
        ensure!(
            self.maximum_slope_c_per_min.is_finite()
                && (0.0..=0.5).contains(&self.maximum_slope_c_per_min),
            "invalid settling tolerance"
        );
        ensure!(
            self.cpu_limit_c.is_finite() && (55.0..=90.0).contains(&self.cpu_limit_c),
            "CPU qualification limit outside 55..90 C"
        );
        ensure!(
            self.gpu_limit_c.is_finite() && (55.0..=80.0).contains(&self.gpu_limit_c),
            "GPU qualification limit outside 55..80 C"
        );
        Ok(())
    }
}

fn eligible(point: &Candidate, policy: &Policy) -> bool {
    point.valid
        && point.repetitions >= policy.minimum_repetitions
        && point.cpu_pwm <= 200
        && point.gpu_pwm <= 200
        && point.noise_dbfs.is_finite()
        && point.noise_dbfs <= 0.0
        && point.cpu_temp_c.is_finite()
        && (0.0..=policy.cpu_limit_c).contains(&point.cpu_temp_c)
        && point
            .gpu_temp_c
            .is_none_or(|t| t.is_finite() && (0.0..=policy.gpu_limit_c).contains(&t))
        && point
            .cpu_rpm
            .is_some_and(|rpm| point.cpu_pwm == 0 || rpm > 0)
        && point
            .gpu_rpm
            .is_some_and(|rpm| point.gpu_pwm == 0 || rpm > 0)
        && point.power_w.is_finite()
        && point.power_w > 0.0
        && point.slope_c_per_min.is_finite()
        && point.slope_c_per_min.abs() <= policy.maximum_slope_c_per_min
}

/// Find the quieter side of a substantial noise increase with little cooling
/// return. An unmeasured or continuously effective loudest point never wins.
pub fn select_ceiling<'a>(points: &'a [Candidate], policy: &Policy) -> Result<&'a Candidate> {
    policy.validate()?;
    let mut points: Vec<_> = points.iter().filter(|p| eligible(p, policy)).collect();
    points.sort_by_key(|p| {
        (
            u16::from(p.cpu_pwm) + u16::from(p.gpu_pwm),
            p.cpu_pwm,
            p.gpu_pwm,
        )
    });
    for pair in points.windows(2) {
        let (quiet, loud) = (pair[0], pair[1]);
        if loud.cpu_pwm < quiet.cpu_pwm
            || loud.gpu_pwm < quiet.gpu_pwm
            || (loud.cpu_pwm == quiet.cpu_pwm && loud.gpu_pwm == quiet.gpu_pwm)
        {
            continue;
        }
        // A changed stored table is not independent proof of fan response.
        let cpu_response = loud
            .cpu_rpm
            .zip(quiet.cpu_rpm)
            .is_some_and(|(l, q)| l >= q && (loud.cpu_pwm == quiet.cpu_pwm || l > q));
        let gpu_response = loud
            .gpu_rpm
            .zip(quiet.gpu_rpm)
            .is_some_and(|(l, q)| l >= q && (loud.gpu_pwm == quiet.gpu_pwm || l > q));
        if !cpu_response || !gpu_response {
            continue;
        }
        if (loud.power_w - quiet.power_w).abs() / quiet.power_w
            > policy.maximum_power_difference_fraction
        {
            continue;
        }
        let matched = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(a), Some(b)) => {
                a.is_finite()
                    && b.is_finite()
                    && a > 0.0
                    && (a - b).abs() / a <= policy.maximum_power_difference_fraction
            }
            (None, None) => true,
            _ => false,
        };
        if !matched(quiet.cpu_power_w, loud.cpu_power_w)
            || !matched(quiet.gpu_power_w, loud.gpu_power_w)
        {
            continue;
        }
        let cpu_gain = quiet.cpu_temp_c - loud.cpu_temp_c;
        let gpu_gain = match (quiet.gpu_temp_c, loud.gpu_temp_c) {
            (Some(a), Some(b)) => a - b,
            (None, None) => 0.0,
            _ => continue,
        };
        if loud.noise_dbfs - quiet.noise_dbfs >= policy.minimum_noise_step_db
            && cpu_gain.abs().max(gpu_gain.abs()) <= policy.maximum_marginal_cooling_c
        {
            return Ok(quiet);
        }
    }
    anyhow::bail!("inconclusive: no repeated, matched-power acoustic/cooling knee")
}

/// Equivalent continuous digital level: average energies, not logarithms.
pub fn equivalent_level(levels: &[f64]) -> Result<f64> {
    ensure!(
        !levels.is_empty() && levels.iter().all(|x| x.is_finite() && *x <= 0.0),
        "invalid acoustic levels"
    );
    let energy = levels
        .iter()
        .map(|db| 10.0_f64.powf(*db / 10.0))
        .sum::<f64>()
        / levels.len() as f64;
    ensure!(
        energy > 0.0 && energy.is_finite(),
        "invalid acoustic energy"
    );
    Ok(10.0 * energy.log10())
}

/// Every matched repetition must support the selected knee, not just pooled
/// averages. Ordered repeats are deliberately kept separate by the collector.
pub fn verify_repeated_knee(
    quiet: &[Candidate],
    loud: &[Candidate],
    policy: &Policy,
) -> Result<()> {
    ensure!(
        quiet.len() == loud.len() && quiet.len() >= policy.minimum_repetitions as usize,
        "inconclusive: insufficient paired repetitions"
    );
    for (quiet, loud) in quiet.iter().zip(loud) {
        let mut pair = [quiet.clone(), loud.clone()];
        for candidate in &mut pair {
            candidate.repetitions = policy.minimum_repetitions;
        }
        let selected = select_ceiling(&pair, policy)?;
        ensure!(
            selected.cpu_pwm == quiet.cpu_pwm && selected.gpu_pwm == quiet.gpu_pwm,
            "inconclusive: repeated knee selected a different pair"
        );
    }
    Ok(())
}

/// Intersect measured joint operating points across qualification scopes. A
/// CPU minimum from one trial and GPU minimum from another are not a measured
/// pair and must never be synthesized into a supposedly learned envelope.
pub fn common_ceiling(cases: &[Vec<Candidate>], policy: &Policy) -> Result<(u8, u8)> {
    ensure!(!cases.is_empty(), "no acoustic cases supplied");
    let ceilings = cases
        .iter()
        .map(|c| select_ceiling(c, policy))
        .collect::<Result<Vec<_>>>()?;
    let mut common: Vec<_> = cases[0]
        .iter()
        .filter(|candidate| {
            let pair = (candidate.cpu_pwm, candidate.gpu_pwm);
            ceilings
                .iter()
                .all(|ceiling| pair.0 <= ceiling.cpu_pwm && pair.1 <= ceiling.gpu_pwm)
                && cases.iter().all(|case| {
                    case.iter()
                        .any(|p| (p.cpu_pwm, p.gpu_pwm) == pair && eligible(p, policy))
                })
        })
        .collect();
    common.sort_by_key(|p| u16::from(p.cpu_pwm) + u16::from(p.gpu_pwm));
    let selected = common.last().ok_or_else(|| {
        anyhow::anyhow!("inconclusive: no common measured joint acoustic ceiling")
    })?;
    Ok((selected.cpu_pwm, selected.gpu_pwm))
}
