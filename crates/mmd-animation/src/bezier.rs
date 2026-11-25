//! Cubic Bézier sampling for VMD curves.
//!
//! `x` is the fraction of the way from the previous key to this key. The
//! returned value is the interpolation weight. A curve whose controls are all
//! zero is treated as linear, matching keys whose 64 bytes are cleared.

use mmd_core::Bezier;

/// Sample a curve. `x` is clamped to 0..=1.
pub fn sample(curve: Bezier, x: f32) -> f32 {
    let x = if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let x1 = unit(curve.x1);
    let y1 = unit(curve.y1);
    let x2 = unit(curve.x2);
    let y2 = unit(curve.y2);
    if x1 == 0.0 && y1 == 0.0 && x2 == 0.0 && y2 == 0.0 {
        return x;
    }
    let mut low = 0.0;
    let mut high = 1.0;
    let mut t = x;
    for _ in 0..24 {
        t = (low + high) * 0.5;
        let at = component(t, x1, x2);
        if at < x {
            low = t;
        } else {
            high = t;
        }
    }
    component(t, y1, y2)
}

fn unit(value: u8) -> f32 {
    f32::from(value) / 127.0
}

fn component(t: f32, p1: f32, p2: f32) -> f32 {
    let u = 1.0 - t;
    3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_linear_curve_stays_near_the_diagonal() {
        let weight = sample(Bezier::LINEAR, 0.5);
        assert!((weight - 0.5).abs() < 0.02, "{weight}");
        assert!((sample(Bezier::LINEAR, 0.0) - 0.0).abs() < 0.001);
        assert!((sample(Bezier::LINEAR, 1.0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn cleared_curve_is_linear() {
        let curve = Bezier {
            x1: 0,
            y1: 0,
            x2: 0,
            y2: 0,
        };
        assert!((sample(curve, 0.25) - 0.25).abs() < 0.0001);
    }
}
