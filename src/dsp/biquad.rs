//! Second-order IIR sections with CamillaDSP's coefficient formulas (the RBJ
//! audio EQ cookbook), and the Butterworth and Linkwitz–Riley cascades of its
//! `BiquadCombo`. Sections run in transposed direct form II.

use std::f64::consts::PI;

/// Coefficients normalized so that a0 is 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coefficients {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coefficients {
    fn normalized(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Coefficients { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }

    pub fn peaking(rate: f64, freq: f64, q: f64, gain_db: f64) -> Self {
        let (sn, cs) = (2.0 * PI * freq / rate).sin_cos();
        let ampl = 10f64.powf(gain_db / 40.0);
        let alpha = sn / (2.0 * q);
        Self::normalized(
            1.0 + alpha * ampl,
            -2.0 * cs,
            1.0 - alpha * ampl,
            1.0 + alpha / ampl,
            -2.0 * cs,
            1.0 - alpha / ampl,
        )
    }

    pub fn allpass(rate: f64, freq: f64, q: f64) -> Self {
        let (sn, cs) = (2.0 * PI * freq / rate).sin_cos();
        let alpha = sn / (2.0 * q);
        Self::normalized(1.0 - alpha, -2.0 * cs, 1.0 + alpha, 1.0 + alpha, -2.0 * cs, 1.0 - alpha)
    }

    pub fn pass(pass: Pass, rate: f64, freq: f64, q: f64) -> Self {
        let (sn, cs) = (2.0 * PI * freq / rate).sin_cos();
        let alpha = sn / (2.0 * q);
        let (b0, b1) = match pass {
            Pass::High => ((1.0 + cs) / 2.0, -(1.0 + cs)),
            Pass::Low => ((1.0 - cs) / 2.0, 1.0 - cs),
        };
        Self::normalized(b0, b1, b0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha)
    }

    /// First-order section (bilinear transform), b2 and a2 zero.
    pub fn pass_first_order(pass: Pass, rate: f64, freq: f64) -> Self {
        let k = (PI * freq / rate).tan();
        let alpha = 1.0 + k;
        let (b0, b1) = match pass {
            Pass::High => (1.0 / alpha, -1.0 / alpha),
            Pass::Low => (k / alpha, k / alpha),
        };
        Coefficients { b0, b1, b2: 0.0, a1: -(1.0 - k) / alpha, a2: 0.0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    High,
    Low,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Butterworth,
    LinkwitzRiley,
}

/// The sections of a `BiquadCombo` of `order`, or `None` for an order
/// CamillaDSP rejects (zero, or odd for Linkwitz–Riley).
pub fn combo(family: Family, pass: Pass, rate: f64, freq: f64, order: usize) -> Option<Vec<Coefficients>> {
    let section = |q| Coefficients::pass(pass, rate, freq, q);
    match family {
        Family::Butterworth if order > 0 => {
            let mut sections: Vec<_> = butterworth_q(order).into_iter().map(section).collect();
            if !order.is_multiple_of(2) {
                sections.push(Coefficients::pass_first_order(pass, rate, freq));
            }
            Some(sections)
        }
        Family::LinkwitzRiley if order > 0 && order.is_multiple_of(2) => {
            let half = butterworth_q(order / 2);
            let mut sections: Vec<_> = half.iter().chain(&half).copied().map(section).collect();
            // Two equal first-order sections make one second-order section with Q 0.5.
            if !(order / 2).is_multiple_of(2) {
                sections.push(section(0.5));
            }
            Some(sections)
        }
        _ => None,
    }
}

/// Q of each second-order section of a Butterworth filter of `order`.
fn butterworth_q(order: usize) -> Vec<f64> {
    (0..order / 2)
        .map(|k| 1.0 / (2.0 * (PI / order as f64 * (k as f64 + 0.5)).sin()))
        .collect()
}

pub struct Biquad {
    c: Coefficients,
    s1: f64,
    s2: f64,
}

impl Biquad {
    pub fn new(c: Coefficients) -> Self {
        Biquad { c, s1: 0.0, s2: 0.0 }
    }

    pub fn process(&mut self, block: &mut [f64]) {
        let c = self.c;
        for sample in block {
            let x = *sample;
            let y = c.b0 * x + self.s1;
            self.s1 = c.b1 * x - c.a1 * y + self.s2;
            self.s2 = c.b2 * x - c.a2 * y;
            *sample = y;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48_000.0;

    /// Complex response of the cascade at `freq`, as (re, im).
    fn response(sections: &[Coefficients], freq: f64) -> (f64, f64) {
        let w = 2.0 * PI * freq / RATE;
        let mul = |a: (f64, f64), b: (f64, f64)| (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0);
        let z1 = (w.cos(), -w.sin());
        let z2 = ((2.0 * w).cos(), -(2.0 * w).sin());
        let mut total = (1.0, 0.0);
        for c in sections {
            let num = (c.b0 + c.b1 * z1.0 + c.b2 * z2.0, c.b1 * z1.1 + c.b2 * z2.1);
            let den = (1.0 + c.a1 * z1.0 + c.a2 * z2.0, c.a1 * z1.1 + c.a2 * z2.1);
            let norm = den.0 * den.0 + den.1 * den.1;
            total = mul(total, ((num.0 * den.0 + num.1 * den.1) / norm, (num.1 * den.0 - num.0 * den.1) / norm));
        }
        total
    }

    fn db(sections: &[Coefficients], freq: f64) -> f64 {
        let (re, im) = response(sections, freq);
        10.0 * (re * re + im * im).log10()
    }

    #[test]
    fn peaking_has_its_gain_at_its_frequency() {
        let peq = [Coefficients::peaking(RATE, 1000.0, 2.0, -6.0)];
        assert!((db(&peq, 1000.0) + 6.0).abs() < 1e-9);
        assert!(db(&peq, 20.0).abs() < 0.01);
        assert!(db(&peq, 15_000.0).abs() < 0.01);
    }

    #[test]
    fn allpass_is_flat() {
        let ap = [Coefficients::allpass(RATE, 80.0, 0.7)];
        for freq in [10.0, 80.0, 500.0, 20_000.0] {
            assert!(db(&ap, freq).abs() < 1e-9);
        }
    }

    #[test]
    fn butterworth_is_3_db_down_at_its_frequency_and_follows_the_analog_slope() {
        for order in [1, 2, 3, 4, 8] {
            let hp = combo(Family::Butterworth, Pass::High, RATE, 100.0, order).unwrap();
            assert!((db(&hp, 100.0) + 3.0103).abs() < 1e-3, "order {order}");
            let analog = -10.0 * (1.0 + 2f64.powi(2 * order as i32)).log10();
            assert!((db(&hp, 50.0) - analog).abs() < 0.05, "order {order}");
            let lp = combo(Family::Butterworth, Pass::Low, RATE, 100.0, order).unwrap();
            assert!((db(&lp, 100.0) + 3.0103).abs() < 1e-3, "order {order}");
        }
    }

    #[test]
    fn linkwitz_riley_halves_sum_to_an_allpass() {
        for order in [2, 4, 8] {
            let hp = combo(Family::LinkwitzRiley, Pass::High, RATE, 80.0, order).unwrap();
            let lp = combo(Family::LinkwitzRiley, Pass::Low, RATE, 80.0, order).unwrap();
            assert_eq!(hp.len(), order / 2);
            assert!((db(&hp, 80.0) + 6.0206).abs() < 1e-3, "order {order}");
            for freq in [20.0, 80.0, 300.0, 5000.0] {
                let (a, b) = (response(&hp, freq), response(&lp, freq));
                // LR2 sums with the high-pass inverted.
                let sign = if order % 4 == 2 { -1.0 } else { 1.0 };
                let sum = (a.0 * sign + b.0, a.1 * sign + b.1);
                let mag = (sum.0 * sum.0 + sum.1 * sum.1).sqrt();
                assert!((mag - 1.0).abs() < 1e-9, "order {order} at {freq} Hz: {mag}");
            }
        }
    }

    #[test]
    fn rejects_orders_camilladsp_rejects() {
        assert!(combo(Family::LinkwitzRiley, Pass::High, RATE, 80.0, 3).is_none());
        assert!(combo(Family::Butterworth, Pass::High, RATE, 80.0, 0).is_none());
    }

    #[test]
    fn processing_matches_the_difference_equation() {
        let c = Coefficients::peaking(RATE, 1000.0, 1.0, 6.0);
        let input: Vec<f64> = (0..64).map(|n| ((n * 7919) % 13) as f64 - 6.0).collect();
        let mut expected = vec![0.0; input.len()];
        for n in 0..input.len() {
            let x = |k: usize| if n >= k { input[n - k] } else { 0.0 };
            let y = |k: usize, e: &[f64]| if n >= k { e[n - k] } else { 0.0 };
            expected[n] = c.b0 * x(0) + c.b1 * x(1) + c.b2 * x(2) - c.a1 * y(1, &expected) - c.a2 * y(2, &expected);
        }
        let mut biquad = Biquad::new(c);
        let mut output = input.clone();
        let (a, b) = output.split_at_mut(10);
        biquad.process(a);
        biquad.process(b);
        for (got, want) in output.iter().zip(&expected) {
            assert!((got - want).abs() < 1e-12);
        }
    }
}
