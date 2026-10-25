//! Animation curves of the pill (pure logic, no GTK).
//!
//! Equivalents of the CSS transitions: `cubic-bezier`, `ease`, `ease-in-out`,
//! `think` animation and smoothing of the bars (70 ms transition).

/// Appearance curve: `cubic-bezier(0.2, 0.9, 0.25, 1.1)` (overshoots 1).
pub const APPEAR_CURVE: [f64; 4] = [0.2, 0.9, 0.25, 1.1];
/// Curve of the `pop` animation: `cubic-bezier(0.2, 0.9, 0.25, 1.3)`.
pub const POP_CURVE: [f64; 4] = [0.2, 0.9, 0.25, 1.3];
/// CSS `ease`.
pub const EASE: [f64; 4] = [0.25, 0.1, 0.25, 1.0];
/// CSS `ease-in-out`.
pub const EASE_IN_OUT: [f64; 4] = [0.42, 0.0, 0.58, 1.0];

/// Appearance / disappearance duration (ms).
pub const APPEAR_MS: f64 = 300.0;
/// Duration of the opacity at appearance (ms).
pub const FADE_MS: f64 = 220.0;
/// Period of the `think` animation (ms).
pub const THINK_MS: f64 = 900.0;
/// Duration of the `pop` animation (ms).
pub const POP_MS: f64 = 320.0;
/// Time constant of the bar smoothing: 95 % of the distance covered in 70 ms.
pub const BAR_TAU_MS: f64 = 70.0 / 3.0;

/// Evaluates a CSS Bézier curve `cubic-bezier(x1, y1, x2, y2)` at `t` (normalised time, clamped to [0, 1]).
pub fn cubic_bezier(curve: [f64; 4], t: f64) -> f64 {
    let [x1, y1, x2, y2] = curve;
    let t = t.clamp(0.0, 1.0);
    if t == 0.0 || t == 1.0 {
        return t;
    }
    let sample = |a: f64, b: f64, s: f64| {
        let u = 1.0 - s;
        3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
    };
    // Solving x(s) = t: bisection (x1, x2 in [0, 1] so x is monotonic).
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    let mut s = t;
    for _ in 0..40 {
        let x = sample(x1, x2, s);
        if (x - t).abs() < 1e-7 {
            break;
        }
        if x < t {
            lo = s;
        } else {
            hi = s;
        }
        s = (lo + hi) / 2.0;
    }
    sample(y1, y2, s)
}

/// Triangular wave of the `think` animation on phase `p` (in periods): 0 → 1 → 0.
pub fn triangle(p: f64) -> f64 {
    let f = p.rem_euclid(1.0);
    1.0 - (2.0 * f - 1.0).abs()
}

/// Value of the `think` animation in [0, 1] at instant `time_ms`, with an offset `offset_ms`
/// (equivalent of a negative `animation-delay`: the bar is already advanced by `offset_ms`).
pub fn think_wave(time_ms: f64, offset_ms: f64) -> f64 {
    let p = (time_ms + offset_ms) / THINK_MS;
    cubic_bezier(EASE_IN_OUT, triangle(p))
}

/// Exponential interpolation towards the target, independent of the frame rate.
pub fn smooth_toward(current: f64, target: f64, dt_ms: f64, tau_ms: f64) -> f64 {
    if dt_ms <= 0.0 {
        return current;
    }
    let k = 1.0 - (-dt_ms / tau_ms).exp();
    current + (target - current) * k
}

/// Linear interpolation.
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn bezier_endpoints_and_linear() {
        assert_eq!(cubic_bezier(APPEAR_CURVE, 0.0), 0.0);
        assert_eq!(cubic_bezier(APPEAR_CURVE, 1.0), 1.0);
        // Almost linear curve: (1/3, 1/3, 2/3, 2/3) is exactly the diagonal.
        assert!(close(
            cubic_bezier([1.0 / 3.0, 1.0 / 3.0, 2.0 / 3.0, 2.0 / 3.0], 0.37),
            0.37
        ));
    }

    #[test]
    fn appear_curve_overshoots() {
        let max = (0..=100)
            .map(|i| cubic_bezier(APPEAR_CURVE, f64::from(i) / 100.0))
            .fold(0.0_f64, f64::max);
        assert!(max > 1.0, "the expected overshoot is missing: {max}");
        assert!(max < 1.2);
        // Known value: the curve is already well advanced at mid-course.
        assert!(cubic_bezier(APPEAR_CURVE, 0.5) > 0.8);
    }

    #[test]
    fn ease_in_out_is_symmetric() {
        assert!(close(cubic_bezier(EASE_IN_OUT, 0.5), 0.5));
        let a = cubic_bezier(EASE_IN_OUT, 0.2);
        let b = cubic_bezier(EASE_IN_OUT, 0.8);
        assert!(close(a + b, 1.0));
    }

    #[test]
    fn triangle_wave() {
        assert!(close(triangle(0.0), 0.0));
        assert!(close(triangle(0.25), 0.5));
        assert!(close(triangle(0.5), 1.0));
        assert!(close(triangle(1.0), 0.0));
        assert!(close(triangle(-0.25), 0.5));
    }

    #[test]
    fn think_is_periodic_and_offset_by_half_period() {
        assert!(close(think_wave(0.0, 0.0), 0.0));
        assert!(close(think_wave(450.0, 0.0), 1.0));
        assert!(close(think_wave(900.0, 0.0), 0.0));
        // Odd bars: shifted by half a period (opposite phase).
        assert!(close(think_wave(0.0, 450.0), 1.0));
        assert!(close(
            think_wave(123.0, 0.0),
            think_wave(123.0 + 900.0, 0.0)
        ));
    }

    #[test]
    fn smoothing_reaches_95_percent_in_70ms() {
        let v = smooth_toward(0.0, 1.0, 70.0, BAR_TAU_MS);
        assert!((v - 0.95).abs() < 0.01, "{v}");
        // Splitting the step does not change the result.
        let mut w = 0.0;
        for _ in 0..7 {
            w = smooth_toward(w, 1.0, 10.0, BAR_TAU_MS);
        }
        assert!(close(v, w));
        assert_eq!(smooth_toward(0.3, 1.0, 0.0, BAR_TAU_MS), 0.3);
    }
}
