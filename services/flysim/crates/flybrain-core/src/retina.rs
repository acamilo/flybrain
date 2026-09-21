//! Retina projection: an RGBA image of any size onto a population of input columns.
//!
//! Ports `model/retina.ts`. The column bounding box is recomputed on every call, which is
//! redundant in practice but is what the original kernel did and therefore part of the arithmetic.

use crate::jsmath::{js_max, js_min, js_round};

/// Membrane drive per unit luminance, and the frame size `setVisualFrame` assumes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetinaConfig {
    pub gain: f64,
    pub width: u32,
    pub height: u32,
}

/// Original constants: a Game Boy sized frame at 0.20 drive per unit luminance.
pub const DEFAULT_RETINA_CONFIG: RetinaConfig = RetinaConfig {
    gain: 0.20,
    width: 160,
    height: 144,
};

impl Default for RetinaConfig {
    fn default() -> Self {
        DEFAULT_RETINA_CONFIG
    }
}

/// Retina column geometry, normally a view onto a dataset's visual arrays.
#[derive(Debug, Clone, Copy)]
pub struct RetinaColumns<'a> {
    /// Interleaved x,y coordinates in dataset units, length >= 2 * count.
    pub xy: &'a [f32],
    /// 0 = left (mirrored on X), 1 = right.
    pub hemisphere: &'a [u8],
    /// Number of columns to project.
    pub count: usize,
}

/// Rec. 709 luminance weights, matching the original kernel.
const RED: f64 = 0.2126;
const GREEN: f64 = 0.7152;
const BLUE: f64 = 0.0722;

/// Project one RGBA frame onto `out` (drive per column).
///
/// `out` must have at least `columns.count` entries; entries beyond the column count are left
/// untouched.
pub fn project_frame(
    rgba: &[u8],
    width: u32,
    height: u32,
    columns: RetinaColumns<'_>,
    gain: f64,
    out: &mut [f32],
) {
    let count = columns.count;
    let (mut min_x, mut max_x) = (f64::INFINITY, f64::NEG_INFINITY);
    let (mut min_y, mut max_y) = (f64::INFINITY, f64::NEG_INFINITY);
    for index in 0..count {
        let x = f64::from(columns.xy[index * 2]);
        let y = f64::from(columns.xy[index * 2 + 1]);
        min_x = js_min(min_x, x);
        max_x = js_max(max_x, x);
        min_y = js_min(min_y, y);
        max_y = js_max(max_y, y);
    }
    let last_x = f64::from(width) - 1.0;
    let last_y = f64::from(height) - 1.0;
    // `(maxX - minX || 1)`: a zero *or NaN* span falls back to 1, because both are falsy in JS.
    let span_x = or_one(max_x - min_x);
    let span_y = or_one(max_y - min_y);

    for (index, drive) in out.iter_mut().enumerate().take(count) {
        let mut normalized_x = (f64::from(columns.xy[index * 2]) - min_x) / span_x;
        if columns.hemisphere[index] == 0 {
            normalized_x = 1.0 - normalized_x;
        }
        let normalized_y = (f64::from(columns.xy[index * 2 + 1]) - min_y) / span_y;
        let x = js_max(0.0, js_min(last_x, js_round(normalized_x * last_x)));
        let y = js_max(0.0, js_min(last_y, js_round(normalized_y * last_y)));
        let offset = ((y * f64::from(width) + x) * 4.0) as usize;
        let luminance = (f64::from(rgba[offset]) * RED
            + f64::from(rgba[offset + 1]) * GREEN
            + f64::from(rgba[offset + 2]) * BLUE)
            / 255.0;
        *drive = (luminance * gain) as f32;
    }
}

/// JavaScript `value || 1`, for the degenerate-axis guard.
#[inline]
fn or_one(value: f64) -> f64 {
    if value == 0.0 || value.is_nan() {
        1.0
    } else {
        value
    }
}
