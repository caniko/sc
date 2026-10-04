use sc_core::firmware_curve::{
    minimum_pwm, CurveSpec, CurveTransaction, Field, RawPoint, Readback, Session, State,
};

fn spec() -> CurveSpec {
    CurveSpec {
        effective_points: 3,
        maximum_temperature_mc: 100_000,
        temperature_step_mc: 1000,
        minimum_percent: 30,
    }
}

fn points() -> Vec<RawPoint> {
    vec![
        RawPoint {
            temperature_mc: 0,
            pwm: 80,
        },
        RawPoint {
            temperature_mc: 40_000,
            pwm: 120,
        },
        RawPoint {
            temperature_mc: 80_000,
            pwm: 255,
        },
    ]
}

fn session() -> Session {
    Session {
        owner: "test-controller".into(),
        reset_epoch: 1,
    }
}

fn transaction() -> CurveTransaction {
    CurveTransaction::new(spec().validate(&points()).unwrap(), session()).unwrap()
}

fn stage(tx: &mut CurveTransaction, skip: Option<(usize, Field)>) {
    for (index, point) in points().iter().enumerate() {
        for (field, value) in [
            (Field::Temperature, point.temperature_mc),
            (Field::Pwm, point.pwm),
        ] {
            if skip != Some((index, field)) {
                tx.stage(&session(), index, field, value).unwrap();
            }
        }
    }
}

#[test]
fn requires_the_full_effective_table_without_padding() {
    assert!(spec().validate(&points()[..2]).is_err());
    assert_eq!(spec().validate(&points()).unwrap().points().len(), 3);
}

#[test]
fn validates_raw_bounds_quantization_and_late_order_before_preparation() {
    for point in [
        RawPoint {
            temperature_mc: -1,
            pwm: 80,
        },
        RawPoint {
            temperature_mc: 101_000,
            pwm: 80,
        },
        RawPoint {
            temperature_mc: 40_001,
            pwm: 80,
        },
        RawPoint {
            temperature_mc: 0,
            pwm: -1,
        },
        RawPoint {
            temperature_mc: 0,
            pwm: 256,
        },
        RawPoint {
            temperature_mc: 0,
            pwm: 650,
        },
        RawPoint {
            temperature_mc: 0,
            pwm: 76,
        },
    ] {
        let mut curve = points();
        curve[0] = point;
        assert!(spec().validate(&curve).is_err(), "{point:?}");
    }
    for point in [
        RawPoint {
            temperature_mc: 40_000,
            pwm: 255,
        },
        RawPoint {
            temperature_mc: 80_000,
            pwm: 80,
        },
    ] {
        let mut curve = points();
        curve[2] = point;
        assert!(spec().validate(&curve).is_err());
    }
}

#[test]
fn floor_conversion_never_rounds_below_the_requested_percent() {
    assert_eq!(minimum_pwm(30).unwrap(), 77);
    for percent in 0..=100 {
        let pwm = minimum_pwm(percent).unwrap();
        assert!(i64::from(pwm) * 100 >= percent * 255);
        if pwm > 0 {
            assert!(i64::from(pwm - 1) * 100 < percent * 255);
        }
    }
    assert!(minimum_pwm(-1).is_err());
    assert!(minimum_pwm(101).is_err());
}

#[test]
fn zero_temperature_is_a_typed_field_not_a_pwm_selector() {
    let mut tx = transaction();
    stage(&mut tx, None);
    tx.stage(&session(), 0, Field::Temperature, 0).unwrap();
    let curve = tx.prepare(&session()).unwrap();
    assert_eq!(curve.points()[0].temperature_mc, 0);
    assert_eq!(curve.points()[0].pwm, 80);
    assert_eq!(tx.state(), State::Prepared);
}

#[test]
fn every_missing_field_and_final_pair_only_blocks_preparation() {
    for index in 0..3 {
        for field in [Field::Temperature, Field::Pwm] {
            let mut tx = transaction();
            stage(&mut tx, Some((index, field)));
            assert!(tx.prepare(&session()).is_err());
            assert_eq!(tx.state(), State::Staging);
        }
    }
    let mut tx = transaction();
    tx.stage(&session(), 2, Field::Temperature, 80_000).unwrap();
    tx.stage(&session(), 2, Field::Pwm, 255).unwrap();
    assert!(tx.prepare(&session()).is_err());
}

#[test]
fn competing_owner_stale_epoch_and_invalid_index_cannot_stage() {
    let mut tx = transaction();
    for key in [
        Session {
            owner: "other".into(),
            reset_epoch: 1,
        },
        Session {
            owner: session().owner,
            reset_epoch: 0,
        },
    ] {
        assert!(tx.stage(&key, 0, Field::Pwm, 90).is_err());
    }
    assert!(tx.stage(&session(), 3, Field::Pwm, 90).is_err());
    assert!(tx.prepare(&session()).is_err());
}

#[test]
fn cache_missing_malformed_or_failed_readback_requires_recovery_not_fake_rollback() {
    for (ok, provenance, observed) in [
        (true, Readback::DriverCache, Some(points())),
        (true, Readback::Firmware, None),
        (true, Readback::Firmware, Some(points()[..2].to_vec())),
        (false, Readback::Firmware, Some(points())),
    ] {
        let mut tx = transaction();
        stage(&mut tx, None);
        tx.prepare(&session()).unwrap();
        assert!(!tx
            .finish(&session(), ok, provenance, observed.as_deref())
            .unwrap());
        assert_eq!(tx.state(), State::RecoveryRequired);
        assert!(tx.last_reported().is_none());
        assert_eq!(tx.original().points().len(), 3);
        assert!(tx.prepare(&session()).is_err());
    }
}

#[test]
fn firmware_match_is_reported_acceptance_not_physical_proof() {
    let mut tx = transaction();
    stage(&mut tx, None);
    tx.prepare(&session()).unwrap();
    assert!(tx
        .finish(&session(), true, Readback::Firmware, Some(&points()))
        .unwrap());
    assert_eq!(tx.state(), State::FirmwareReported);
    assert!(tx.last_reported().is_some());
    assert!(tx.stage(&session(), 0, Field::Pwm, 90).is_err());
}

#[test]
fn reset_discards_staging_and_reported_acceptance_until_rediscovery() {
    let mut tx = transaction();
    stage(&mut tx, None);
    tx.prepare(&session()).unwrap();
    tx.finish(&session(), true, Readback::Firmware, Some(&points()))
        .unwrap();
    tx.reset(2).unwrap();
    assert_eq!(tx.state(), State::Invalidated);
    assert!(tx.last_reported().is_none());
    assert!(tx.prepare(&session()).is_err());
    assert!(tx
        .stage(
            &Session {
                reset_epoch: 2,
                ..session()
            },
            0,
            Field::Pwm,
            90
        )
        .is_err());
    assert!(tx.reset(2).is_err());
}

#[test]
fn last_point_first_and_duplicate_fields_do_not_implicitly_commit() {
    let mut tx = transaction();
    tx.stage(&session(), 2, Field::Pwm, 255).unwrap();
    tx.stage(&session(), 2, Field::Temperature, 80_000).unwrap();
    tx.stage(&session(), 2, Field::Pwm, 255).unwrap();
    assert_eq!(tx.state(), State::Staging);
    assert!(tx.prepare(&session()).is_err());
    stage(&mut tx, None);
    assert_eq!(tx.prepare(&session()).unwrap().points().len(), 3);
}

#[test]
fn late_invalid_order_blocks_preparation_until_corrected() {
    let mut tx = transaction();
    stage(&mut tx, None);
    tx.stage(&session(), 2, Field::Temperature, 0).unwrap();
    assert!(tx.prepare(&session()).is_err());
    assert_eq!(tx.state(), State::Staging);
    assert!(tx
        .finish(&session(), true, Readback::Firmware, Some(&points()))
        .is_err());
    tx.stage(&session(), 2, Field::Temperature, 80_000).unwrap();
    tx.prepare(&session()).unwrap();
}

#[test]
fn valid_but_different_readback_and_uncertain_commit_cannot_be_retried() {
    let mut tx = transaction();
    stage(&mut tx, None);
    tx.prepare(&session()).unwrap();
    let mut observed = points();
    observed[0].pwm = 90;
    assert!(!tx
        .finish(&session(), true, Readback::Firmware, Some(&observed))
        .unwrap());
    assert_eq!(tx.state(), State::RecoveryRequired);
    assert!(tx.stage(&session(), 0, Field::Pwm, 80).is_err());
    assert!(tx
        .finish(&session(), true, Readback::Firmware, Some(&points()))
        .is_err());
    assert!(tx.last_reported().is_none());
}

#[test]
fn owner_and_epoch_checks_also_protect_prepare_and_finish() {
    let mut tx = transaction();
    stage(&mut tx, None);
    let other = Session {
        owner: "other".into(),
        ..session()
    };
    assert!(tx.prepare(&other).is_err());
    assert_eq!(tx.state(), State::Staging);
    tx.prepare(&session()).unwrap();
    assert!(tx
        .finish(&other, true, Readback::Firmware, Some(&points()))
        .is_err());
    assert_eq!(tx.state(), State::Prepared);
    assert!(tx
        .finish(&session(), true, Readback::Firmware, Some(&points()))
        .unwrap());
}

#[test]
fn reset_also_discards_an_incomplete_or_prepared_transaction() {
    for prepared in [false, true] {
        let mut tx = transaction();
        stage(&mut tx, None);
        if prepared {
            tx.prepare(&session()).unwrap();
        }
        tx.reset(2).unwrap();
        assert!(tx
            .finish(&session(), true, Readback::Firmware, Some(&points()))
            .is_err());
        assert_eq!(tx.state(), State::Invalidated);
    }
}

#[test]
fn invalid_abi_bounds_and_empty_owner_are_rejected() {
    for invalid in [
        CurveSpec {
            effective_points: 0,
            ..spec()
        },
        CurveSpec {
            maximum_temperature_mc: -1,
            ..spec()
        },
        CurveSpec {
            temperature_step_mc: 0,
            ..spec()
        },
        CurveSpec {
            minimum_percent: 101,
            ..spec()
        },
    ] {
        assert!(invalid.validate(&points()).is_err());
    }
    assert!(CurveTransaction::new(
        spec().validate(&points()).unwrap(),
        Session {
            owner: " ".into(),
            ..session()
        }
    )
    .is_err());
}
