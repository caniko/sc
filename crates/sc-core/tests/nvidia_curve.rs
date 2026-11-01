use sc_core::nvidia::*;

fn pt(temp: u32, percent: u32) -> NvidiaPoint {
    NvidiaPoint { temp, percent }
}

fn gpu(num: u32, disabled: &[u32]) -> NvidiaGpu {
    NvidiaGpu {
        device: "0000:01:00.0".into(),
        hysteresis: 0,
        fans: (0..num)
            .map(|index| NvidiaFan {
                index,
                disabled: disabled.contains(&index),
                curve: vec![pt(40, 20), pt(80, 100)],
            })
            .collect(),
    }
}

#[test]
fn maps_curve_and_clamps_to_range() {
    let g = gpu(2, &[]);
    let mut held = None;
    assert_eq!(plan(&g, &mut held, Some(60.0), (30, 100))[0], (0, 60));
    assert_eq!(plan(&g, &mut held, Some(20.0), (30, 100))[0], (0, 30)); // curve 20 -> min 30
    assert_eq!(plan(&g, &mut held, Some(90.0), (30, 80))[0], (0, 80)); // curve 100 -> max 80
}

#[test]
fn disabled_fan_gets_minimum_and_failsafe_is_max() {
    let g = gpu(2, &[1]);
    let mut held = None;
    assert_eq!(
        plan(&g, &mut held, Some(90.0), (30, 100)),
        [(0, 100), (1, 30)]
    );
    assert_eq!(plan(&g, &mut held, None, (30, 100)), [(0, 100), (1, 30)]);
    assert_eq!(plan(&g, &mut held, None, (0, 100)), [(0, 100), (1, 0)]);
}

#[test]
fn hysteresis_holds_small_drops() {
    let mut g = gpu(1, &[]);
    g.hysteresis = 5;
    let mut held = None;
    plan(&g, &mut held, Some(70.0), (0, 100));
    assert_eq!(plan(&g, &mut held, Some(67.0), (0, 100))[0].1, 80);
    assert_eq!(plan(&g, &mut held, Some(60.0), (0, 100))[0].1, 60);
}

#[test]
fn startup_checks() {
    assert!(check_hardware(&gpu(3, &[2]), 3).is_ok());
    assert!(check_hardware(&gpu(3, &[]), 2).is_err()); // index 2 >= num_fans
    let err = check_hardware(&gpu(1, &[0]), 1).unwrap_err().to_string();
    assert!(err.contains("ganged"), "{err}");
}

#[test]
fn rejects_all_fans_disabled() {
    assert!(validate(&[gpu(2, &[0, 1])]).is_err());
    assert!(validate(&[gpu(2, &[1])]).is_ok());
}

#[test]
fn rejects_non_monotonic_curves() {
    let mut g = gpu(1, &[]);
    assert!(validate(&[g.clone()]).is_ok());
    g.fans[0].curve = vec![pt(40, 50), pt(80, 20)];
    assert!(validate(&[g.clone()]).is_err());
    g.fans[0].curve = vec![pt(40, 20), pt(40, 30)];
    assert!(validate(&[g]).is_err());
}
