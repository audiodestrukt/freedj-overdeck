//! The mixer's signal path, free of any plugin API so it can be tested alone.
//!
//! The EQ is a three-band isolator, as on a club mixer: the signal is split
//! into low / mid / high with Linkwitz-Riley 4th-order crossovers and each
//! band gets its own gain, down to a full kill.  With every band at 0 dB the
//! bands sum back to the input's magnitude exactly (the low band also goes
//! through the upper crossover's all-pass, so all three share one phase).

use std::f32::consts::{FRAC_PI_2, PI};

/// The crossover points, Hz (a common club-mixer split).
pub const LOW_MID_HZ: f32 = 300.0;
pub const MID_HIGH_HZ: f32 = 3000.0;

/// An EQ setting at or below this kills the band (gain 0).
pub const KILL_DB: f32 = -26.0;

pub fn db_to_gain(db: f32) -> f32 { if db <= KILL_DB { 0.0 } else { 10f32.powf(db / 20.0) } }

/// A biquad (RBJ cookbook), transposed direct form II.
#[derive(Clone, Copy, Default)]
struct Biquad { b0: f32, b1: f32, b2: f32, a1: f32, a2: f32, z1: f32, z2: f32 }

impl Biquad {
    fn butterworth(rate: f32, hz: f32, high: bool) -> Biquad {
        let w = 2.0 * PI * hz / rate;
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        let (b0, b1, b2) = if high { ((1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0) } else { ((1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0) };
        Biquad { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: -2.0 * c / a0, a2: (1.0 - alpha) / a0, z1: 0.0, z2: 0.0 }
    }
    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// A Linkwitz-Riley 4th-order filter: two Butterworth biquads in a row.
#[derive(Clone, Copy, Default)]
struct Lr4([Biquad; 2]);

impl Lr4 {
    fn new(rate: f32, hz: f32, high: bool) -> Lr4 { let b = Biquad::butterworth(rate, hz, high); Lr4([b, b]) }
    #[inline]
    fn run(&mut self, x: f32) -> f32 { let y = self.0[0].run(x); self.0[1].run(y) }
}

/// One channel's three-band isolator.
#[derive(Clone, Copy, Default)]
pub struct Isolator {
    low: Lr4,
    rest: Lr4,
    mid: Lr4,
    high: Lr4,
    // The upper crossover's all-pass, for the low band (LP + HP of it).
    ap_lp: Lr4,
    ap_hp: Lr4,
}

impl Isolator {
    pub fn new(rate: f32) -> Isolator {
        Isolator {
            low: Lr4::new(rate, LOW_MID_HZ, false),
            rest: Lr4::new(rate, LOW_MID_HZ, true),
            mid: Lr4::new(rate, MID_HIGH_HZ, false),
            high: Lr4::new(rate, MID_HIGH_HZ, true),
            ap_lp: Lr4::new(rate, MID_HIGH_HZ, false),
            ap_hp: Lr4::new(rate, MID_HIGH_HZ, true),
        }
    }

    /// One sample through, with the bands' gains (linear).
    #[inline]
    pub fn run(&mut self, x: f32, low: f32, mid: f32, high: f32) -> f32 {
        let l = self.low.run(x);
        let l = self.ap_lp.run(l) + self.ap_hp.run(l);
        let r = self.rest.run(x);
        let m = self.mid.run(r);
        let h = self.high.run(r);
        low * l + mid * m + high * h
    }
}

/// The crossfader's gains for A and B at position `x` (0 = A, 1 = B):
/// constant power, so a blend through the middle doesn't dip.
pub fn crossfader(x: f32) -> (f32, f32) {
    let x = x.clamp(0.0, 1.0);
    ((x * FRAC_PI_2).cos(), (x * FRAC_PI_2).sin())
}

/// A channel fader's gain: its position squared (a smooth taper, 0 at the
/// bottom, unity at the top).
pub fn fader(pos: f32) -> f32 { let p = pos.clamp(0.0, 1.0); p * p }

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    /// The RMS of the isolator's output for a sine at `hz`, after it settles.
    fn rms_through(hz: f32, g: (f32, f32, f32)) -> f32 {
        let mut iso = Isolator::new(RATE);
        let n = RATE as usize;
        let mut acc = 0.0f64;
        for i in 0..n {
            let x = (2.0 * PI * hz * i as f32 / RATE).sin();
            let y = iso.run(x, g.0, g.1, g.2);
            if i >= n / 2 { acc += f64::from(y * y); }
        }
        ((acc / (n / 2) as f64).sqrt() * std::f64::consts::SQRT_2) as f32
    }

    #[test]
    fn flat_eq_passes_every_frequency_at_unity() {
        for hz in [40.0, 150.0, 300.0, 900.0, 3000.0, 8000.0, 15000.0] {
            let g = rms_through(hz, (1.0, 1.0, 1.0));
            let db = 20.0 * g.log10();
            assert!(db.abs() < 0.1, "{hz} Hz comes out {db:+.2} dB with the EQ flat");
        }
    }

    #[test]
    fn each_kill_takes_out_its_own_band_only() {
        let db = |hz, g| 20.0 * rms_through(hz, g).log10();
        assert!(db(60.0, (0.0, 1.0, 1.0)) < -40.0, "low kill: 60 Hz gone");
        assert!(db(1000.0, (1.0, 0.0, 1.0)) < -30.0, "mid kill: 1 kHz gone");
        assert!(db(12000.0, (1.0, 1.0, 0.0)) < -40.0, "high kill: 12 kHz gone");
        assert!(db(1000.0, (0.0, 1.0, 0.0)).abs() < 0.5, "1 kHz untouched by the low and high kills");
        assert!(db(60.0, (1.0, 0.0, 0.0)).abs() < 0.5, "60 Hz untouched by the mid and high kills");
    }

    #[test]
    fn the_crossfader_is_one_side_at_each_end_and_constant_power_between() {
        assert_eq!(crossfader(0.0).1, 0.0);
        assert!(crossfader(1.0).0.abs() < 1e-6);
        for x in [0.1, 0.5, 0.8] {
            let (a, b) = crossfader(x);
            assert!((a * a + b * b - 1.0).abs() < 1e-5);
        }
        assert_eq!(db_to_gain(KILL_DB), 0.0);
        assert!((db_to_gain(0.0) - 1.0).abs() < 1e-6);
    }
}
