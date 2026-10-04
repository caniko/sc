//! Pure full-table validation/staging for a future qualified firmware backend.
//!
//! No device I/O or capability grant: the caller must audit its actual ABI,
//! units, effective length and getter behavior. Session checks are a model of
//! cooperative ownership, not exclusion of kernel or unrelated root writers.
//! The current daemon does not use this contract to enable GPU control.

use anyhow::{ensure, Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawPoint {
    pub temperature_mc: i64,
    pub pwm: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub temperature_mc: i64,
    pub pwm: u8,
}

/// Caller-supplied ABI bounds, not discovered hardware capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CurveSpec {
    pub effective_points: usize,
    pub maximum_temperature_mc: i64,
    pub temperature_step_mc: i64,
    pub minimum_percent: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Curve {
    spec: CurveSpec,
    points: Vec<Point>,
}

impl Curve {
    pub fn points(&self) -> &[Point] {
        &self.points
    }
}

impl CurveSpec {
    pub fn validate(self, raw: &[RawPoint]) -> Result<Curve> {
        ensure!(
            self.effective_points > 0,
            "effective point count must be positive"
        );
        ensure!(
            self.maximum_temperature_mc >= 0,
            "invalid temperature bound"
        );
        ensure!(
            self.temperature_step_mc > 0,
            "temperature step must be positive"
        );
        ensure!(
            self.minimum_percent <= 100,
            "minimum percent outside 0..100"
        );
        ensure!(
            raw.len() == self.effective_points,
            "complete effective table required"
        );
        let mut points: Vec<Point> = Vec::with_capacity(raw.len());
        for point in raw {
            self.temperature(point.temperature_mc)?;
            let pwm = self.pwm(point.pwm)?;
            if let Some(previous) = points.last() {
                ensure!(
                    point.temperature_mc > previous.temperature_mc,
                    "temperatures must strictly increase"
                );
                ensure!(pwm >= previous.pwm, "PWM must not decrease");
            }
            points.push(Point {
                temperature_mc: point.temperature_mc,
                pwm,
            });
        }
        Ok(Curve { spec: self, points })
    }

    fn temperature(self, value: i64) -> Result<()> {
        ensure!(
            (0..=self.maximum_temperature_mc).contains(&value),
            "temperature outside ABI bounds"
        );
        ensure!(
            value % self.temperature_step_mc == 0,
            "explicit temperature quantization required"
        );
        Ok(())
    }

    fn pwm(self, value: i64) -> Result<u8> {
        let pwm = u8::try_from(value).context("PWM outside 0..255")?;
        ensure!(
            u16::from(pwm) * 100 / 255 >= u16::from(self.minimum_percent),
            "effective PWM below reviewed floor"
        );
        Ok(pwm)
    }
}

/// Smallest scalar 0..255 value not below an integer percentage floor.
pub fn minimum_pwm(percent: i64) -> Result<u8> {
    ensure!((0..=100).contains(&percent), "percent outside 0..100");
    Ok(u8::try_from((percent * 255 + 99) / 100)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Temperature,
    Pwm,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub owner: String,
    pub reset_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readback {
    DriverCache,
    Firmware,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Staging,
    Prepared,
    FirmwareReported,
    RecoveryRequired,
    Invalidated,
}

/// A saved original curve is restoration data, not proof of active policy.
pub struct CurveTransaction {
    original: Curve,
    session: Session,
    staged: Vec<[Option<i64>; 2]>,
    prepared: Option<Curve>,
    last_reported: Option<Curve>,
    state: State,
}

impl CurveTransaction {
    pub fn new(original: Curve, session: Session) -> Result<Self> {
        ensure!(!session.owner.trim().is_empty(), "explicit owner required");
        Ok(Self {
            staged: vec![[None, None]; original.points.len()],
            original,
            session,
            prepared: None,
            last_reported: None,
            state: State::Staging,
        })
    }

    fn check(&self, session: &Session, state: State) -> Result<()> {
        ensure!(session == &self.session, "wrong owner or stale reset epoch");
        ensure!(
            self.state == state,
            "transaction is {:?}, expected {state:?}",
            self.state
        );
        Ok(())
    }

    pub fn stage(
        &mut self,
        session: &Session,
        index: usize,
        field: Field,
        value: i64,
    ) -> Result<()> {
        self.check(session, State::Staging)?;
        let slot = match field {
            Field::Temperature => {
                self.original.spec.temperature(value)?;
                0
            }
            Field::Pwm => {
                self.original.spec.pwm(value)?;
                1
            }
        };
        self.staged
            .get_mut(index)
            .context("point index outside effective table")?[slot] = Some(value);
        Ok(())
    }

    /// Prepare only a complete fresh table; no submission occurs here.
    pub fn prepare(&mut self, session: &Session) -> Result<&Curve> {
        self.check(session, State::Staging)?;
        let raw = self
            .staged
            .iter()
            .map(|point| {
                Ok(RawPoint {
                    temperature_mc: point[0].context("missing temperature field")?,
                    pwm: point[1].context("missing PWM field")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let curve = self.original.spec.validate(&raw)?;
        self.prepared = Some(curve);
        self.state = State::Prepared;
        self.prepared.as_ref().context("prepared table unavailable")
    }

    /// Caller-audited firmware readback can establish reported acceptance only.
    /// Cache equality, missing data and uncertain submission require recovery;
    /// keeping `original` does not establish rollback or physical fan response.
    pub fn finish(
        &mut self,
        session: &Session,
        transport_ok: bool,
        provenance: Readback,
        observed: Option<&[RawPoint]>,
    ) -> Result<bool> {
        self.check(session, State::Prepared)?;
        self.state = State::RecoveryRequired;
        let observed = observed.and_then(|raw| self.original.spec.validate(raw).ok());
        if transport_ok
            && provenance == Readback::Firmware
            && observed.as_ref() == self.prepared.as_ref()
        {
            self.last_reported = observed;
            self.state = State::FirmwareReported;
            self.staged.clear();
            return Ok(true);
        }
        Ok(false)
    }

    /// A new monotonically increasing local reset epoch requires rediscovery.
    pub fn reset(&mut self, epoch: u64) -> Result<()> {
        ensure!(
            epoch > self.session.reset_epoch,
            "newer reset epoch required"
        );
        self.session.reset_epoch = epoch;
        self.staged.clear();
        self.prepared = None;
        self.last_reported = None;
        self.state = State::Invalidated;
        Ok(())
    }

    pub fn state(&self) -> State {
        self.state
    }
    pub fn original(&self) -> &Curve {
        &self.original
    }
    pub fn last_reported(&self) -> Option<&Curve> {
        self.last_reported.as_ref()
    }
}
