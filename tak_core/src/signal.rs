//! Read-only display signal processing, independent of the viewer and file storage.
//!
//! Cutoffs are the one-pass design cutoffs: no forward/backward correction. Zero-phase
//! filtering uses odd reflection, padlen = 3 * actual transfer-function order, and
//! steady-state section initial conditions (the same convention as scipy sosfilt_zi).
//! See docs/device-filter.md for the chain, gap and display contracts.

use std::f64::consts::PI;

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Demean,
    LowPass {
        cutoff_hz: f64,
        order: usize,
        zero_phase: bool,
    },
    HighPass {
        cutoff_hz: f64,
        order: usize,
        zero_phase: bool,
    },
    BandPass {
        low_hz: f64,
        high_hz: f64,
        order: usize,
        zero_phase: bool,
    },
    BandStop {
        low_hz: f64,
        high_hz: f64,
        order: usize,
        zero_phase: bool,
    },
    Rectify,
    MovingRms {
        window_ms: f64,
    },
}

#[derive(Clone, Debug)]
pub struct Processed {
    pub values: Vec<f64>,
    /// Finite runs left entirely raw because at least one zero-phase filter needs padding.
    pub skipped_runs: usize,
}

/// Normalized second-order section, a[0] = 1. A first-order section has b[2] = a[2] = 0.
#[derive(Clone, Copy, Debug)]
pub struct Section {
    pub b: [f64; 3],
    pub a: [f64; 3],
}

#[derive(Clone, Debug)]
pub struct Filter {
    pub sections: Vec<Section>,
    pub order: usize,
}

fn cutoff(fc: f64, fs: f64) -> Result<(), String> {
    if !fs.is_finite() || fs <= 0.0 {
        return Err("sampling rate must be positive".into());
    }
    if !fc.is_finite() || fc <= 0.0 {
        return Err("cutoff must be > 0 Hz".into());
    }
    if fc >= fs / 2.0 {
        return Err(format!("must be < fs/2 = {} Hz", fs / 2.0));
    }
    Ok(())
}
fn order(n: usize) -> Result<(), String> {
    if !(1..=8).contains(&n) {
        Err("order must be 1–8".into())
    } else {
        Ok(())
    }
}
fn band(lo: f64, hi: f64, fs: f64) -> Result<(), String> {
    cutoff(lo, fs)?;
    cutoff(hi, fs)?;
    if lo >= hi {
        return Err("Low must be < High".into());
    }
    Ok(())
}

/// Bilinear transform of the Butterworth prototype with prewarped cutoff.
pub fn butterworth(cutoff_hz: f64, fs: f64, n: usize, high_pass: bool) -> Result<Filter, String> {
    cutoff(cutoff_hz, fs)?;
    order(n)?;
    let k = (PI * cutoff_hz / fs).tan();
    let mut sections = Vec::with_capacity((n + 1) / 2);
    if n % 2 == 1 {
        let b0 = if high_pass {
            1.0 / (1.0 + k)
        } else {
            k / (1.0 + k)
        };
        sections.push(Section {
            b: [b0, if high_pass { -b0 } else { b0 }, 0.0],
            a: [1.0, (k - 1.0) / (k + 1.0), 0.0],
        });
    }
    // Low-Q sections first. Each complex-conjugate pole pair has damping 2*sin(theta).
    for j in (0..n / 2).rev() {
        let damping = 2.0 * (PI * (2 * j + 1) as f64 / (2 * n) as f64).sin();
        let den = 1.0 + damping * k + k * k;
        let b0 = if high_pass { 1.0 / den } else { k * k / den };
        sections.push(Section {
            b: [b0, if high_pass { -2.0 * b0 } else { 2.0 * b0 }, b0],
            a: [
                1.0,
                2.0 * (k * k - 1.0) / den,
                (1.0 - damping * k + k * k) / den,
            ],
        });
    }
    Ok(Filter { sections, order: n })
}

/// Band-pass = high-pass(Low) followed by low-pass(High), one combined SOS filter.
pub fn band_pass(low_hz: f64, high_hz: f64, fs: f64, n: usize) -> Result<Filter, String> {
    band(low_hz, high_hz, fs)?;
    let mut hp = butterworth(low_hz, fs, n, true)?;
    hp.sections
        .extend(butterworth(high_hz, fs, n, false)?.sections);
    hp.order = 2 * n;
    Ok(hp)
}

#[derive(Clone, Copy, Debug)]
struct Complex {
    re: f64,
    im: f64,
}
impl Complex {
    fn add(self, o: Self) -> Self {
        Self {
            re: self.re + o.re,
            im: self.im + o.im,
        }
    }
    fn sub(self, o: Self) -> Self {
        Self {
            re: self.re - o.re,
            im: self.im - o.im,
        }
    }
    fn scale(self, s: f64) -> Self {
        Self {
            re: self.re * s,
            im: self.im * s,
        }
    }
    fn mul(self, o: Self) -> Self {
        Self {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
    fn div(self, o: Self) -> Self {
        let d = o.re * o.re + o.im * o.im;
        Self {
            re: (self.re * o.re + self.im * o.im) / d,
            im: (self.im * o.re - self.re * o.im) / d,
        }
    }
    fn sqrt(self) -> Self {
        let r = self.re.hypot(self.im);
        Self {
            re: ((r + self.re) * 0.5).max(0.0).sqrt(),
            im: ((r - self.re) * 0.5).max(0.0).sqrt().copysign(self.im),
        }
    }
}

/// Standard low-pass-prototype to band-stop transform; design N produces digital order 2N.
pub fn band_stop(low_hz: f64, high_hz: f64, fs: f64, n: usize) -> Result<Filter, String> {
    band(low_hz, high_hz, fs)?;
    order(n)?;
    // Normalize analog frequencies by 2*fs before applying z=(1+s)/(1-s).
    let lo = (PI * low_hz / fs).tan();
    let hi = (PI * high_hz / fs).tan();
    let bw = hi - lo;
    let w02 = lo * hi;
    let one = Complex { re: 1.0, im: 0.0 };
    let mut poles = Vec::with_capacity(2 * n);
    for j in 0..n {
        let theta = PI * (2 * j + 1) as f64 / (2 * n) as f64;
        let p = Complex {
            re: -theta.sin(),
            im: theta.cos(),
        };
        let t = Complex { re: bw, im: 0.0 }.div(p).scale(0.5);
        let r = t.mul(t).sub(Complex { re: w02, im: 0.0 }).sqrt();
        for s in [t.add(r), t.sub(r)] {
            poles.push(one.add(s).div(one.sub(s)));
        }
    }
    let zero = one
        .add(Complex {
            re: 0.0,
            im: w02.sqrt(),
        })
        .div(one.sub(Complex {
            re: 0.0,
            im: w02.sqrt(),
        }));
    let mut sections = Vec::with_capacity(n);
    // Each positive-imaginary pole pairs with its conjugate. Pair remaining real poles.
    let mut real = Vec::new();
    for p in poles {
        if p.im > 1e-12 {
            sections.push(Section {
                b: [1.0, -2.0 * zero.re, 1.0],
                a: [1.0, -2.0 * p.re, p.re * p.re + p.im * p.im],
            });
        } else if p.im.abs() <= 1e-12 {
            real.push(p.re);
        }
    }
    real.sort_by(f64::total_cmp);
    for pair in real.chunks_exact(2) {
        sections.push(Section {
            b: [1.0, -2.0 * zero.re, 1.0],
            a: [1.0, -pair[0] - pair[1], pair[0] * pair[1]],
        });
    }
    if sections.len() != n {
        return Err("unable to pair band-stop poles".into());
    }
    sections.sort_by(|a, b| a.a[2].total_cmp(&b.a[2]));
    // Normalize each section at DC. This distributes gain and avoids unnecessary peaks.
    for s in &mut sections {
        let gain = s.a.iter().sum::<f64>() / s.b.iter().sum::<f64>();
        for b in &mut s.b {
            *b *= gain;
        }
    }
    Ok(Filter {
        sections,
        order: 2 * n,
    })
}

/// scipy.signal.iirnotch convention: -3 dB bandwidth = centre_hz / Q.
pub fn notch(centre_hz: f64, q: f64, fs: f64) -> Result<Filter, String> {
    cutoff(centre_hz, fs)?;
    if !q.is_finite() || q <= 0.0 {
        return Err("Q must be > 0".into());
    }
    let w0 = 2.0 * PI * centre_hz / fs;
    let bw = w0 / q;
    if bw >= PI {
        return Err("notch bandwidth must be < fs/2".into());
    }
    let beta = (bw * 0.5).tan();
    let gain = 1.0 / (1.0 + beta);
    Ok(Filter {
        sections: vec![Section {
            b: [gain, -2.0 * gain * w0.cos(), gain],
            a: [1.0, -2.0 * gain * w0.cos(), 2.0 * gain - 1.0],
        }],
        order: 2,
    })
}

impl Step {
    pub fn filter(&self, fs: f64) -> Result<Option<(Filter, bool)>, String> {
        Ok(match *self {
            Self::LowPass {
                cutoff_hz,
                order,
                zero_phase,
            } => Some((butterworth(cutoff_hz, fs, order, false)?, zero_phase)),
            Self::HighPass {
                cutoff_hz,
                order,
                zero_phase,
            } => Some((butterworth(cutoff_hz, fs, order, true)?, zero_phase)),
            Self::BandPass {
                low_hz,
                high_hz,
                order,
                zero_phase,
            } => Some((band_pass(low_hz, high_hz, fs, order)?, zero_phase)),
            Self::BandStop {
                low_hz,
                high_hz,
                order,
                zero_phase,
            } => Some((band_stop(low_hz, high_hz, fs, order)?, zero_phase)),
            Self::MovingRms { window_ms } => {
                if !window_ms.is_finite()
                    || window_ms <= 0.0
                    || !(fs * window_ms / 1000.0).is_finite()
                {
                    return Err("window must be finite and > 0 ms".into());
                }
                None
            }
            _ => None,
        })
    }
}

pub fn validate(steps: &[Step], fs: f64) -> Result<(), String> {
    if !fs.is_finite() || fs <= 0.0 {
        return Err("sampling rate must be positive".into());
    }
    for step in steps {
        step.filter(fs)?;
    }
    Ok(())
}

fn check_cancel(cancelled: &impl Fn() -> bool) -> Result<(), String> {
    if cancelled() {
        Err("cancelled".into())
    } else {
        Ok(())
    }
}

fn filter_pass(
    values: &mut [f64],
    filter: &Filter,
    steady: bool,
    cancelled: &impl Fn() -> bool,
) -> Result<(), String> {
    let initial = values[0];
    let mut scale = 1.0;
    let mut states: Vec<[f64; 2]> = filter
        .sections
        .iter()
        .map(|s| {
            let g = s.b.iter().sum::<f64>() / s.a.iter().sum::<f64>();
            let zi = if steady {
                [
                    (g - s.b[0]) * scale * initial,
                    (s.b[2] - s.a[2] * g) * scale * initial,
                ]
            } else {
                [0.0; 2]
            };
            scale *= g;
            zi
        })
        .collect();
    for (i, value) in values.iter_mut().enumerate() {
        if i % 1024 == 0 {
            check_cancel(cancelled)?;
        }
        for (s, z) in filter.sections.iter().zip(&mut states) {
            let x = *value;
            *value = s.b[0] * x + z[0];
            z[0] = s.b[1] * x - s.a[1] * *value + z[1];
            z[1] = s.b[2] * x - s.a[2] * *value;
        }
        if !value.is_finite() {
            return Err("filter produced a non-finite result".into());
        }
    }
    Ok(())
}

/// Filter one finite run. Returns None for runs too short for the requested odd padding.
pub fn apply_filter(
    values: &[f64],
    filter: &Filter,
    zero_phase: bool,
) -> Result<Option<Vec<f64>>, String> {
    apply_filter_cancellable(values, filter, zero_phase, &|| false)
}
fn apply_filter_cancellable(
    values: &[f64],
    filter: &Filter,
    zero_phase: bool,
    cancelled: &impl Fn() -> bool,
) -> Result<Option<Vec<f64>>, String> {
    if values.is_empty() {
        return Ok(Some(Vec::new()));
    }
    if !values.iter().all(|v| v.is_finite()) {
        return Err("filter input must be one finite run".into());
    }
    let pad = if zero_phase { 3 * filter.order } else { 0 };
    if zero_phase && values.len() <= pad {
        return Ok(None);
    }
    let mut y = Vec::with_capacity(values.len() + 2 * pad);
    for j in (1..=pad).rev() {
        y.push(2.0 * values[0] - values[j]);
    }
    y.extend_from_slice(values);
    for j in 1..=pad {
        y.push(2.0 * values[values.len() - 1] - values[values.len() - 1 - j]);
    }
    filter_pass(&mut y, filter, zero_phase, cancelled)?;
    if zero_phase {
        y.reverse();
        filter_pass(&mut y, filter, true, cancelled)?;
        y.reverse();
    }
    Ok(Some(y[pad..pad + values.len()].to_vec()))
}

pub fn process(values: &[f64], fs: f64, steps: &[Step]) -> Result<Processed, String> {
    process_cancellable(values, fs, steps, || false)
}

/// Cancellation is checked between steps/runs and at least every 1024 samples in IIR work.
/// Neither success nor cancellation modifies the caller's raw samples.
pub fn process_cancellable(
    values: &[f64],
    fs: f64,
    steps: &[Step],
    cancelled: impl Fn() -> bool,
) -> Result<Processed, String> {
    validate(steps, fs)?;
    check_cancel(&cancelled)?;
    let filters: Vec<_> = steps
        .iter()
        .map(|s| s.filter(fs))
        .collect::<Result<_, _>>()?;
    let pad = filters
        .iter()
        .flatten()
        .filter(|(_, zero)| *zero)
        .map(|(f, _)| 3 * f.order)
        .max()
        .unwrap_or(0);
    let mut runs = Vec::new();
    let mut at = 0;
    let mut skipped_runs = 0;
    while at < values.len() {
        if !values[at].is_finite() {
            at += 1;
            continue;
        }
        let start = at;
        while at < values.len() && values[at].is_finite() {
            at += 1;
        }
        let skip = pad > 0 && at - start <= pad;
        skipped_runs += usize::from(skip);
        if !skip {
            runs.push(start..at);
        }
    }
    let mut output = values.to_vec();
    for (step, filter) in steps.iter().zip(&filters) {
        check_cancel(&cancelled)?;
        // Demean always uses every finite sample in the whole record, including short runs.
        let mean = if matches!(step, Step::Demean) {
            let (sum, n) = output
                .iter()
                .filter(|v| v.is_finite())
                .fold((0.0, 0usize), |(s, n), &v| (s + v, n + 1));
            if n > 0 {
                sum / n as f64
            } else {
                0.0
            }
        } else {
            0.0
        };
        for range in &runs {
            check_cancel(&cancelled)?;
            let x = &mut output[range.clone()];
            if let Some((f, zero)) = filter {
                let result = apply_filter_cancellable(x, f, *zero, &cancelled)?
                    .expect("short runs excluded before processing");
                x.copy_from_slice(&result);
            } else {
                match *step {
                    Step::Demean => {
                        for v in x {
                            *v -= mean;
                        }
                    }
                    Step::Rectify => {
                        for v in x {
                            *v = v.abs();
                        }
                    }
                    Step::MovingRms { window_ms } => {
                        let window = (fs * window_ms / 1000.0)
                            .round()
                            .max(1.0)
                            .min(usize::MAX as f64) as usize;
                        let left = window / 2;
                        let right = (window - 1) / 2;
                        let mut prefix = Vec::with_capacity(x.len() + 1);
                        prefix.push(0.0);
                        for (i, &v) in x.iter().enumerate() {
                            if i % 1024 == 0 {
                                check_cancel(&cancelled)?;
                            }
                            prefix.push(prefix[i] + v * v);
                        }
                        for (i, v) in x.iter_mut().enumerate() {
                            let a = i.saturating_sub(left);
                            let b = i
                                .saturating_add(right)
                                .saturating_add(1)
                                .min(prefix.len() - 1);
                            *v = ((prefix[b] - prefix[a]).max(0.0) / (b - a) as f64).sqrt();
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
        if output
            .iter()
            .zip(values)
            .any(|(&o, &raw)| raw.is_finite() && !o.is_finite())
        {
            return Err("processing produced a non-finite result".into());
        }
    }
    Ok(Processed {
        values: output,
        skipped_runs,
    })
}

#[cfg(test)]
mod tests;
