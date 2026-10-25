//! Geometry of the pill (values in logical pixels).

/// Window surface (the pill is drawn in it with margins for the shadow).
pub const SURFACE_W: i32 = 420;
pub const SURFACE_H: i32 = 120;
/// Height of the pill.
pub const PILL_H: f64 = 46.0;
/// Minimum width of the pill.
pub const PILL_MIN_W: f64 = 170.0;
/// Distance between the bottom of the pill and the bottom of the surface.
pub const PILL_BOTTOM: f64 = 14.0;
/// Margin between the surface and the screen edge (layer-shell anchor).
pub const SCREEN_MARGIN: i32 = 12;
pub const PAD_LEFT: f64 = 16.0;
pub const PAD_RIGHT: f64 = 18.0;
pub const GAP: f64 = 12.0;

pub const N_BARS: usize = 22;
pub const BAR_W: f64 = 3.0;
pub const BAR_GAP: f64 = 3.0;
pub const BAR_RADIUS: f64 = 2.0;
pub const BAR_MIN_H: f64 = 4.0;
pub const BAR_RANGE: f64 = 22.0;

/// Disc of the check mark.
pub const CHECK_SIZE: f64 = 22.0;
/// Initial vertical offset of the appearance: 140 % of the pill height.
pub const SLIDE_FACTOR: f64 = 1.4;

pub const LABEL_PX: f64 = 14.0;

/// Width of the bar block: 22 × 3 + 21 × 3 = 129 px.
pub fn bars_width() -> f64 {
    N_BARS as f64 * BAR_W + (N_BARS as f64 - 1.0) * BAR_GAP
}

/// Height of a bar for a level in [0, 1]: 4 + level × 22.
pub fn bar_height(level: f64) -> f64 {
    BAR_MIN_H + level.clamp(0.0, 1.0) * BAR_RANGE
}

/// Effective radius of a bar (CSS reduces the radius if it exceeds half the dimension).
pub fn bar_radius(h: f64) -> f64 {
    BAR_RADIUS.min(BAR_W / 2.0).min(h / 2.0)
}

/// Width of the pill for a label of width `label_w`; `lead_w` is the width of the leading block
/// (bars or check mark).
pub fn pill_width(lead_w: f64, label_w: f64) -> f64 {
    (PAD_LEFT + lead_w + GAP + label_w + PAD_RIGHT).max(PILL_MIN_W)
}

/// Rectangle of the pill (x, y, width, height) in a `w × h` surface, centred, anchored at the bottom.
pub fn pill_rect(surface_w: f64, surface_h: f64, pill_w: f64) -> (f64, f64, f64, f64) {
    (
        ((surface_w - pill_w) / 2.0).round(),
        surface_h - PILL_BOTTOM - PILL_H,
        pill_w,
        PILL_H,
    )
}

/// Position of the surface on the screen (X11 fallback): centred, `SCREEN_MARGIN` above the bottom.
pub fn x11_position(mon_x: i32, mon_y: i32, mon_w: i32, mon_h: i32) -> (i32, i32) {
    (
        mon_x + (mon_w - SURFACE_W) / 2,
        mon_y + mon_h - SURFACE_H - SCREEN_MARGIN,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn bars_block_is_129px() {
        assert_eq!(bars_width(), 129.0);
    }

    #[test]
    fn bar_heights_span_4_to_26() {
        assert_eq!(bar_height(0.0), 4.0);
        assert_eq!(bar_height(1.0), 26.0);
        assert_eq!(bar_height(2.0), 26.0);
        assert_eq!(bar_height(-1.0), 4.0);
        assert_eq!(bar_height(0.5), 15.0);
    }

    #[test]
    fn bar_radius_is_clamped() {
        assert_eq!(bar_radius(4.0), 1.5);
        assert_eq!(bar_radius(26.0), 1.5);
    }

    #[test]
    fn pill_width_has_minimum() {
        // Check mark + short label: 88 px < 170 px.
        assert_eq!(pill_width(CHECK_SIZE, 20.0), 170.0);
        assert_eq!(pill_width(bars_width(), 20.0), 195.0);
        // 16 + 129 + 12 + 100 + 18
        assert_eq!(pill_width(bars_width(), 100.0), 275.0);
    }

    #[test]
    fn pill_is_centered_and_anchored_to_bottom() {
        let (x, y, w, h) = pill_rect(420.0, 120.0, 200.0);
        assert_eq!((x, y, w, h), (110.0, 60.0, 200.0, 46.0));
        assert_eq!(y + h + PILL_BOTTOM, 120.0);
    }

    #[test]
    fn x11_position_centers_horizontally() {
        assert_eq!(x11_position(0, 0, 1920, 1080), (750, 948));
        assert_eq!(x11_position(1920, 0, 2560, 1440), (1920 + 1070, 1308));
    }
}
