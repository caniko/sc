# Firmware table contract

`sc-core::firmware_curve` is a pure validation/staging contract for a future
qualified backend. The daemon does not use it to enable a GPU actuator. It
performs no sysfs, mailbox or management-library I/O and grants no hardware
capability.

## Units and validation

`CurveSpec` supplies the audited effective table length, maximum temperature,
temperature step and integer minimum percentage. Temperatures are signed
millidegrees Celsius; input PWM is signed 0..255. The temperature step must be
positive. A temperature of zero identifies a temperature value, never a field
selector.

The contract requires exactly the effective number of points, strictly
increasing temperatures and nondecreasing PWM. It rejects raw out-of-range
values before narrowing, rejects implicit temperature quantization, and never
pads a short table with cached or unknown points. `minimum_pwm` uses ceiling
conversion: for example, a 30% floor requires at least 77/255. This example is
arithmetic, not a verified minimum for a particular board.

The specification is caller evidence. Representation bounds do not establish
safe thermal limits, meaningful firmware responses or a cooling escape ramp.

## State transitions

A transaction preserves a validated original curve as restoration data and
stages every field afresh. Its `Session` identifies a cooperative owner and
reset epoch. Duplicate field writes replace only that field. Writing the final
point does not trigger submission.

`prepare` validates the entire staged table and moves from `Staging` to
`Prepared`. It performs no submission. The backend must separately enforce
the owning driver's PM/reset/lifetime and whole-table writer contract.

`finish` records `FirmwareReported` only when transport succeeded and an
audited firmware readback exactly matches the prepared table. A cached value,
missing/malformed/different readback or transport failure moves to
`RecoveryRequired`. The same transaction cannot blindly retry an uncertain
commit. Saving the original curve does not prove rollback, and reported
acceptance does not prove physical fan response.

`reset` requires an increasing epoch, clears staging and reported acceptance,
and moves to `Invalidated`. Rediscovery requires a new transaction. Session
equality provides no exclusion against unrelated root or kernel writers.

The contract does not restore mode or submit the saved original curve. An
actual backend must preserve and restore the complete original table and mode,
distinguish factory fallback from original-policy restoration, and qualify
physical response and lifecycle behavior independently.

## Telemetry

`sc-hwmon` treats `i915` and `xe` inputs as GPU observations: runtime status
must be `active` before reading RPM or temperature. An inactive or unknown
state produces an unavailable reading with a reason. A read/parse error does
not become zero. A valid zero does not itself identify fan stop or stall.
Intel curve getters are not included in the ASUS-specific curve read path.

Physical fans, tach channels and control domains need independent mapping.
The [Xe ABI](https://raw.githubusercontent.com/torvalds/linux/72d3fcf802c45d00b300f25b848a93c3a2bd7c7e/Documentation/ABI/testing/sysfs-driver-intel-xe-hwmon)
documents DG2 shared-tach wiring where a second channel can be zero while a
physical fan spins. Attribute existence does not establish an actuator.

## Tests

Run `cargo test --workspace --locked --jobs 2` in the approved project
environment. The full-table regressions are in
`crates/sc-core/tests/firmware_curve.rs`; Intel runtime-state tests are in
`crates/sc-hwmon/src/telemetry.rs`. These are offline contract/fixture tests,
not firmware emulation or A770 hardware qualification.
