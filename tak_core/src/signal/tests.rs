use super::*;
use serde_json::Value;

fn near(a: f64, b: f64) {
    assert!(
        (a - b).abs() <= 1e-9 * (1.0 + b.abs()),
        "{a:.17e} != {b:.17e}, error {:.3e}",
        (a - b).abs()
    );
}

#[test]
fn every_order_and_phase_matches_committed_scipy_numbers() {
    let fixture: Value = serde_json::from_str(include_str!(
        "device_filter_scipy.json"
    ))
    .unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 68);
    for case in cases {
        let fs = case["fs"].as_f64().unwrap();
        let n = case["order"].as_u64().unwrap_or(4) as usize;
        let lo = case["low"].as_f64().unwrap();
        let hi = case["high"].as_f64().unwrap_or(0.);
        let zero = case["zero_phase"].as_bool().unwrap();
        let f = match case["kind"].as_str().unwrap() {
            "lp" => butterworth(lo, fs, n, false),
            "hp" => butterworth(lo, fs, n, true),
            "bp" => band_pass(lo, hi, fs, n),
            "bs" => band_stop(lo, hi, fs, n),
            "notch" => notch(lo, case["q"].as_f64().unwrap(), fs),
            _ => unreachable!(),
        }
        .unwrap();
        let x: Vec<_> = case["input"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        let expected = case["output"].as_array().unwrap();
        let y = apply_filter(&x, &f, zero).unwrap().unwrap();
        assert_eq!(y.len(), expected.len(), "{}", case["id"]);
        for (i, (&v, want)) in y.iter().zip(expected).enumerate() {
            let want = want.as_f64().unwrap();
            assert!(
                (v - want).abs() <= 1e-9 * (1. + want.abs()),
                "{} sample {i}: {v:.17e} vs {want:.17e}",
                case["id"]
            );
        }
    }
}

#[test]
fn gaps_keep_their_bits_and_filter_runs_independently() {
    let mut values: Vec<_> = (0..100).map(|i| (i as f64 * 0.2).sin()).collect();
    values.extend([f64::from_bits(0x7ff8_0000_0000_1234), f64::INFINITY]);
    values.extend((0..100).map(|i| 7. + (i as f64 * 0.1).cos()));
    let raw = values.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    let s = Step::LowPass {
        cutoff_hz: 6.,
        order: 4,
        zero_phase: true,
    };
    let result = process(&values, 100., &[s.clone()]).unwrap();
    let a = process(&values[..100], 100., &[s.clone()]).unwrap();
    let b = process(&values[102..], 100., &[s]).unwrap();
    assert_eq!(result.values[..100], a.values);
    assert_eq!(result.values[102..], b.values);
    assert_eq!(result.values[100].to_bits(), raw[100]);
    assert_eq!(result.values[101].to_bits(), raw[101]);
    assert_eq!(
        values.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        raw,
        "input immutable"
    );
}

#[test]
fn short_run_retains_entire_chain_and_reports_once_per_run() {
    let mut x = vec![2., -1., 3., f64::NAN];
    x.extend((0..50).map(|i| i as f64));
    let out = process(
        &x,
        100.,
        &[
            Step::Demean,
            Step::Rectify,
            Step::LowPass {
                cutoff_hz: 6.,
                order: 4,
                zero_phase: true,
            },
        ],
    )
    .unwrap();
    assert_eq!(out.skipped_runs, 1);
    assert_eq!(&out.values[..3], &x[..3]);
    assert!(out.values[3].is_nan());
    assert_ne!(out.values[4], x[4]);
    let boundary = vec![1.; 12];
    assert_eq!(
        process(
            &boundary,
            100.,
            &[Step::LowPass {
                cutoff_hz: 6.,
                order: 4,
                zero_phase: true
            }]
        )
        .unwrap()
        .skipped_runs,
        1
    );
    assert_eq!(
        process(
            &vec![1.; 13],
            100.,
            &[Step::LowPass {
                cutoff_hz: 6.,
                order: 4,
                zero_phase: true
            }]
        )
        .unwrap()
        .skipped_runs,
        0
    );
}

#[test]
fn demean_uses_whole_record_mean_and_rectification_follows_order() {
    let x = [1., f64::NAN, 3., 8.];
    let out = process(&x, 100., &[Step::Demean, Step::Rectify]).unwrap();
    assert_eq!(out.values[0], 3.);
    assert_eq!(out.values[2], 1.);
    assert_eq!(out.values[3], 4.);
    let y = process(&[-2., 1.], 100., &[Step::Rectify, Step::Demean]).unwrap();
    assert_eq!(y.values, [0.5, -0.5]);
    assert!(out.values[1].is_nan());
}

#[test]
fn centered_rms_clips_edges_and_does_not_cross_gaps() {
    let x = [1., 2., 3., 4., f64::NAN, 10., 20.];
    let y = process(&x, 1000., &[Step::MovingRms { window_ms: 4. }])
        .unwrap()
        .values;
    for (actual, want) in [
        (y[0], (5_f64 / 2.).sqrt()),
        (y[1], (14_f64 / 3.).sqrt()),
        (y[2], (30_f64 / 4.).sqrt()),
        (y[3], (29_f64 / 3.).sqrt()),
        (y[5], (500_f64 / 2.).sqrt()),
        (y[6], (500_f64 / 2.).sqrt()),
    ] {
        near(actual, want);
    }
    assert!(y[4].is_nan());
    let z = process(&[-2., 3.], 1000., &[Step::MovingRms { window_ms: 0.1 }]).unwrap();
    assert_eq!(z.values, [2., 3.]);
}

#[test]
fn rejects_every_invalid_parameter_without_mutating_input() {
    let x = [1., 2., 3.];
    for fs in [0., -1., f64::NAN, f64::INFINITY] {
        assert!(process(&x, fs, &[]).is_err());
    }
    for fc in [0., -1., 50., 51., f64::NAN, f64::INFINITY] {
        assert!(butterworth(fc, 100., 4, false).is_err());
    }
    for n in [0, 9, usize::MAX] {
        assert!(butterworth(6., 100., n, false).is_err());
        assert!(band_stop(5., 10., 100., n).is_err());
    }
    for (lo, hi) in [(20., 10.), (10., 10.), (0., 10.), (10., 50.)] {
        assert!(band_pass(lo, hi, 100., 4).is_err());
        assert!(band_stop(lo, hi, 100., 4).is_err());
    }
    for window_ms in [0., -1., f64::NAN, f64::INFINITY] {
        assert!(process(&x, 100., &[Step::MovingRms { window_ms }]).is_err());
    }
    assert!(notch(60., 0., 1000.).is_err());
    assert!(notch(60., 0.01, 1000.).is_err());
    assert_eq!(x, [1., 2., 3.]);
}

#[test]
fn constant_has_no_zero_phase_lp_edge_transient_and_hp_removes_it() {
    for n in 1..=8 {
        let x = vec![7.; 200];
        let lp = apply_filter(&x, &butterworth(6., 100., n, false).unwrap(), true)
            .unwrap()
            .unwrap();
        for value in lp {
            near(value, 7.);
        }
        let hp = apply_filter(&x, &butterworth(6., 100., n, true).unwrap(), true)
            .unwrap()
            .unwrap();
        for value in hp {
            near(value, 0.);
        }
    }
}

#[test]
fn all_nan_empty_identity_and_cancellation_are_explicit() {
    assert!(process(&[], 100., &[Step::Demean])
        .unwrap()
        .values
        .is_empty());
    assert!(process(&[f64::NAN], 100., &[Step::Rectify]).unwrap().values[0].is_nan());
    assert_eq!(process(&[2., -3.], 100., &[]).unwrap().values, [2., -3.]);
    assert_eq!(
        process_cancellable(&[1.; 50], 100., &[Step::Rectify], || true).unwrap_err(),
        "cancelled"
    );
    let calls = std::cell::Cell::new(0);
    let result = process_cancellable(
        &[1.; 5000],
        1000.,
        &[Step::LowPass {
            cutoff_hz: 6.,
            order: 4,
            zero_phase: true,
        }],
        || {
            calls.set(calls.get() + 1);
            calls.get() > 6
        },
    );
    assert_eq!(result.unwrap_err(), "cancelled");
}

#[test]
fn broad_band_stop_real_poles_are_paired_and_stable() {
    for n in 1..=8 {
        let f = band_stop(1., 490., 1000., n).unwrap();
        assert_eq!(f.sections.len(), n);
        let out = apply_filter(&vec![1.; 500], &f, true).unwrap().unwrap();
        for v in out {
            near(v, 1.);
        }
    }
}
