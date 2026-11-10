# Acoustic learning on ASUS firmware-controlled laptops

SmartCool can measure fan noise through an explicit raw internal microphone,
run reversible joint CPU/GPU firmware fan sweeps, and select a repeatable noise
increase with little additional cooling benefit. Audio samples stay in memory;
only digital-level metrics and thermal/fan observations are retained.

These are relative **dBFS** measurements. They are not calibrated dB(A), and a
microphone built into the laptop does not measure the listener's ear directly.
Keep the laptop position, lid, room, microphone port and gain fixed. Speech,
music, movement, clipping, silence, changing gain and capture loss invalidate
learning. Steady external noise still requires a quiet test environment.

## Observe a spin-up cycle

Run capture as the logged-in desktop user. The processed/default microphone or
a playback monitor is rejected; the internal-microphone port must be active.

```sh
sc acoustic capture --source alsa_input.pci-0000_65_00.6.analog-stereo --duration-seconds 30
sc asusd acoustics --microphone alsa_input.pci-0000_65_00.6.analog-stereo --duration-seconds 120
```

The second command emits timestamped microphone metrics, actual RPM and
temperatures, followed by peak noise, equivalent digital level, fan-start
counts and peak RPM. An interrupted observation fails acceptance. A supplied
`--maximum-noise-dbfs` is meaningful only with the same qualified capture setup.

## Guarded learning

Preview first; `--apply` runs the foreground workload and changes curves. Run
the controller with hardware-control privileges and use `--capture-user` to
keep audio access in the desktop user's session. Stop an active
`smartcool-asusd-quiet.service` before benchmarking. On managed hosts, run
mutating experiments inside the host's execution lease as well.

```sh
sc asusd benchmark \
  --microphone alsa_input.pci-0000_65_00.6.analog-stereo --capture-user can \
  --context atlas-adjacent-ac-lid-open --gpu 0000:01:00.0 \
  --levels 70:70,100:100,130:130,160:160 \
  --cpu-ppt 15,25,35 --cpu-stop-c 85 --gpu-stop-c 70 \
  --report /run/nomad-cpu-acoustic.json \
  -- stress-ng --cpu 8 --verify --timeout 2h --metrics-brief
```

Add `--apply` before `--` to execute. For GPU and combined loads, supply an
actually working NVIDIA workload and `--require-gpu-load`. Every NVIDIA display
device must be explicitly monitored, including during a CPU-only trial.
`--gpu-clock-mhz` takes exclusive ownership of the trial's NVIDIA clock request
and resets it to driver defaults on exit; it cannot preserve an unrelated
pre-existing lock. Do not share that control with another clock manager.

The benchmark:

- uses asusd curve transactions rather than manual hwmon PWM;
- preserves the original full-speed emergency endpoint and stops **before**
  its interpolation band, including before the first workload is started;
- rejects requested normal PWM above 200/255;
- checks stored and active driver tables, real RPM, temperature, package power,
  NVIDIA utilization/power/effective clocks, and microphone freshness;
- requires a stable temperature window; a settling timeout is inconclusive;
- repeats at least three times, reversing alternate sweeps;
- matches CPU and GPU heat input separately and requires every repetition to
  support the selected noise/cooling knee;
- stops the workload's process group before restoring fan and power requests
  on normal failure, deadline or interruption;
- preserves newer externally written curves/PPT rather than overwriting them;
- atomically retains a metrics-only report, including failure/restoration status.

Foreground workloads must remain in their process group. Run through a systemd
control group for crash containment when using workloads with descendants.
The direct child also receives a parent-death signal. Firmware retains emergency
cooling if the controller is killed; abrupt death cannot guarantee restoration
of mutable asusd/PPT/clock state. Verify/reapply the declaration after such an
exit before resuming qualification.

An optional monotonically increasing `--throughput-counter` supplies a
workload-owned completed-work counter. Retain workload verification and
throughput results alongside the report. A cooling sweep alone does not prove
acceptable application performance.

```sh
sc acoustic analyze --report /run/nomad-cpu-acoustic.json
sc acoustic envelope --reports /path/to/case-1.json /path/to/case-2.json --out /path/to/common-envelope.json
```

Failed, unrestored, inconsistent or inconclusive reports cannot be promoted by
these commands. Envelope synthesis intersects **measured joint pairs**; it
does not combine independently selected CPU/GPU minima into an untested pair.
The output remains a candidate until all intended scopes pass acceptance.

## Everyday enforcement

`services.smartcool.asusd.quietControl` supplies a watchdog-supervised power
controller. `QuietPolicy.pkl` defines its input; validate it without hardware:

```sh
sc acoustic validate-policy --config /path/to/quiet.pkl
sc asusd supervise --config /path/to/quiet.pkl
```

All three platform profiles must share the learned fan ceiling. The firmware
table stays below it until its narrow emergency interpolation band. Predictive
temperature rise reduces CPU PPT and NVIDIA clocks first; recovery requires a
cool margin and dwell. Emergency fan requests use actual emergency temperature
or a telemetry/fan fault and remain protected by hysteresis across profile
changes. NVIDIA runtime suspension is distinct from missing required telemetry.
Driver/curve/RPM errors do not qualify as silence.

The policy includes measured fan-start PWM, a qualification identifier and exact
DMI, BIOS, kernel, NVIDIA-driver and GPU identities. Identity drift requires
requalification. ASUS PPT limits are discovered and read back; reduced power
uses device minima, not an assumed common minimum for all three controls.
The supervisor owns NVIDIA locked clocks while active. Firmware, asusd and the
supervisor retain distinct responsibilities; the manual SmartCool hwmon daemon
must not control the same fans.

Before runtime promotion, qualify CPU, verified NVIDIA GPU, combined load,
short bursts, recovery, all platform profiles, physical/mobile states,
AC/battery changes, GameMode, suspend/resume and interruption. Verify power
reduction actually reduces heat under load. Test emergency/fault decisions with
simulated telemetry; do not deliberately overheat the machine. Acoustic knees,
safe emergency boundaries and the performance cost are measurement outputs,
not values inferred from a successful build.
