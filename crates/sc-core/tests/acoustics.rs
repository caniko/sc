use sc_core::acoustics::{select_ceiling, Candidate, NoiseWindow, Policy};

fn candidate(pwm: u8, noise_dbfs: f64, temp_c: f64) -> Candidate {
    Candidate {
        cpu_pwm: pwm,
        gpu_pwm: pwm,
        noise_dbfs,
        cpu_temp_c: temp_c,
        gpu_temp_c: Some(temp_c - 10.0),
        cpu_rpm: Some(2000 + u32::from(pwm) * 10),
        gpu_rpm: Some(2000 + u32::from(pwm) * 10),
        power_w: 40.0,
        cpu_power_w: Some(40.0),
        gpu_power_w: None,
        slope_c_per_min: 0.02,
        repetitions: 3,
        valid: true,
    }
}

#[test]
fn microphone_metrics_remove_dc_and_reject_clipping_and_digital_silence() {
    let samples: Vec<f32> = (0..4800)
        .map(|i| 0.1 + if i % 2 == 0 { 0.01 } else { -0.01 })
        .collect();
    let window = NoiseWindow::from_pcm(&samples).unwrap();
    assert!((window.level_dbfs + 40.0).abs() < 0.01);
    assert!(NoiseWindow::from_pcm(&vec![0.0; 4800]).is_err());
    assert!(NoiseWindow::from_pcm(&vec![0.1; 4800]).is_err());
    assert!(NoiseWindow::from_pcm(&[f32::NAN; 4800]).is_err());
    assert!(NoiseWindow::from_pcm(&[1.0; 4800]).is_err());
}

#[test]
fn learns_the_quieter_side_of_a_repeated_noise_and_cooling_knee() {
    let points = [
        candidate(60, -45.0, 76.0),
        candidate(90, -43.0, 71.0),
        candidate(120, -38.0, 70.5),
    ];
    let selected = select_ceiling(&points, &Policy::default()).unwrap();
    assert_eq!((selected.cpu_pwm, selected.gpu_pwm), (90, 90));
}

#[test]
fn missing_or_stalled_fans_and_unsettled_measurements_cannot_win() {
    let mut points = vec![candidate(60, -60.0, 65.0), candidate(90, -43.0, 71.0)];
    points[0].cpu_rpm = None;
    assert!(select_ceiling(&points, &Policy::default()).is_err());
    points[0].cpu_rpm = Some(0);
    assert!(select_ceiling(&points, &Policy::default()).is_err());
    points[0].cpu_rpm = Some(2500);
    points[0].slope_c_per_min = 2.0;
    assert!(select_ceiling(&points, &Policy::default()).is_err());
}

#[test]
fn changed_heat_input_cannot_be_reported_as_better_cooling() {
    let mut points = [candidate(90, -43.0, 71.0), candidate(120, -38.0, 70.5)];
    points[1].power_w = 20.0;
    assert!(select_ceiling(&points, &Policy::default()).is_err());
}

#[test]
fn no_repeatable_knee_is_inconclusive_instead_of_selecting_the_loudest_point() {
    let mut points = [candidate(90, -43.0, 71.0), candidate(120, -42.0, 66.0)];
    assert!(select_ceiling(&points, &Policy::default()).is_err());
    points[1].noise_dbfs = -38.0;
    points[1].cpu_temp_c = 70.5;
    points[1].gpu_temp_c = Some(60.5);
    points[0].repetitions = 1;
    assert!(select_ceiling(&points, &Policy::default()).is_err());
}

#[test]
fn matching_total_power_cannot_hide_a_changed_cpu_gpu_heat_distribution() {
    let mut points = [candidate(90, -43.0, 71.0), candidate(120, -38.0, 70.5)];
    points[0].cpu_power_w = Some(30.0);
    points[0].gpu_power_w = Some(10.0);
    points[1].cpu_power_w = Some(10.0);
    points[1].gpu_power_w = Some(30.0);
    assert!(select_ceiling(&points, &Policy::default()).is_err());
}

#[test]
fn acoustic_averaging_uses_energy_and_preserves_unknown_measurements() {
    let level = sc_core::acoustics::equivalent_level(&[-40.0, -20.0]).unwrap();
    assert!((level + 22.967).abs() < 0.001);
    assert!(sc_core::acoustics::equivalent_level(&[]).is_err());
    assert!(sc_core::acoustics::equivalent_level(&[f64::NAN]).is_err());
}

#[test]
fn pooled_averages_cannot_hide_a_failed_repetition() {
    let quiet = vec![candidate(90, -43.0, 71.0); 3];
    let mut loud = vec![candidate(120, -38.0, 70.5); 3];
    assert!(sc_core::acoustics::verify_repeated_knee(&quiet, &loud, &Policy::default()).is_ok());
    loud[1].noise_dbfs = -42.0;
    assert!(sc_core::acoustics::verify_repeated_knee(&quiet, &loud, &Policy::default()).is_err());
}

#[test]
fn microphone_rejects_nonstationary_background_without_peak_clipping() {
    let samples: Vec<_> = (0..48000)
        .map(|i| {
            let amplitude = if i < 24000 { 0.01 } else { 0.1 };
            if i % 2 == 0 {
                amplitude
            } else {
                -amplitude
            }
        })
        .collect();
    assert!(NoiseWindow::from_pcm(&samples)
        .unwrap_err()
        .to_string()
        .contains("nonstationary"));
}

#[test]
fn shared_ceiling_uses_a_measured_joint_pair_across_scopes() {
    let cases = vec![
        vec![
            candidate(60, -45.0, 75.0),
            candidate(90, -43.0, 71.0),
            candidate(120, -38.0, 70.5)
        ];
        2
    ];
    assert_eq!(
        sc_core::acoustics::common_ceiling(&cases, &Policy::default()).unwrap(),
        (90, 90)
    );
    let mut unmatched = cases;
    unmatched[1].iter_mut().for_each(|c| c.gpu_pwm += 1);
    assert!(sc_core::acoustics::common_ceiling(&unmatched, &Policy::default()).is_err());
}

#[test]
fn a_stored_pwm_change_without_physical_response_is_not_a_noise_knee() {
    let mut points = [candidate(90, -43.0, 71.0), candidate(120, -38.0, 70.5)];
    points[1].cpu_rpm = points[0].cpu_rpm;
    points[1].gpu_rpm = points[0].gpu_rpm;
    assert!(select_ceiling(&points, &Policy::default()).is_err());
}
