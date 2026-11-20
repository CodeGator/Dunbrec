//! A small XY plot widget: linear or logarithmic x axis, 1-2-5 ticks,
//! several coloured series, and a hover cursor with read-outs.

use dunbrec_core::units::format_value;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};

pub struct Series {
    pub label: String,
    pub color: Color32,
    pub points: Vec<(f64, f64)>,
}

pub struct Axis {
    pub label: &'static str,
    pub unit: &'static str,
    pub log: bool,
}

/// Distinct colours for plot series.
pub const SERIES_COLORS: [Color32; 6] = [
    Color32::from_rgb(250, 210, 30),
    Color32::from_rgb(40, 200, 255),
    Color32::from_rgb(255, 90, 200),
    Color32::from_rgb(90, 230, 110),
    Color32::from_rgb(255, 150, 60),
    Color32::from_rgb(180, 140, 255),
];

/// "Nice" tick positions covering `[lo, hi]` (1-2-5 steps).
pub fn linear_ticks(lo: f64, hi: f64, target: usize) -> Vec<f64> {
    if hi.is_nan() || lo.is_nan() || hi <= lo {
        return vec![lo];
    }
    let raw = (hi - lo) / target.max(1) as f64;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * mag);
    // Tolerance so a tick exactly at an end is not lost to rounding
    // (-0.3 / 0.1 is -2.9999999999999996).
    let first = (lo / step - 1e-9).ceil() as i64;
    let last = (hi / step + 1e-9).floor() as i64;
    (first..=last).map(|k| k as f64 * step).collect()
}

/// Decade ticks covering `[lo, hi]` (both > 0).
pub fn log_ticks(lo: f64, hi: f64) -> Vec<f64> {
    let (a, b) = (lo.log10().ceil() as i32, hi.log10().floor() as i32);
    (a..=b).map(|e| 10f64.powi(e)).collect()
}

fn range(series: &[Series], pick: impl Fn(&(f64, f64)) -> f64, log: bool) -> (f64, f64) {
    let values = series
        .iter()
        .flat_map(|s| s.points.iter())
        .map(pick)
        .filter(|v| v.is_finite() && (!log || *v > 0.0));
    let (lo, hi) = values.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
        (lo.min(v), hi.max(v))
    });
    if !lo.is_finite() {
        return if log { (1.0, 10.0) } else { (0.0, 1.0) };
    }
    if hi - lo < 1e-15 * hi.abs().max(1e-12) {
        let pad = if lo == 0.0 { 1.0 } else { lo.abs() * 0.1 };
        return (lo - pad, hi + pad);
    }
    if log {
        (lo, hi)
    } else {
        let pad = (hi - lo) * 0.05;
        (lo - pad, hi + pad)
    }
}

/// Draw the plot; returns the x value under the pointer, if any.
pub fn plot(
    ui: &mut Ui,
    height: f32,
    x_axis: &Axis,
    y_axis: &Axis,
    series: &[Series],
) -> Option<f64> {
    let width = ui.available_width().max(200.0);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter_at(rect);
    let dark = ui.visuals().dark_mode;
    let (bg, grid, text) = if dark {
        (
            Color32::from_rgb(12, 16, 20),
            Color32::from_rgb(45, 55, 65),
            Color32::from_rgb(190, 200, 210),
        )
    } else {
        (
            Color32::from_rgb(252, 252, 250),
            Color32::from_rgb(215, 215, 210),
            Color32::from_rgb(60, 60, 70),
        )
    };
    painter.rect_filled(rect, 4.0, bg);
    let area = Rect::from_min_max(rect.min + vec2(62.0, 10.0), rect.max - vec2(12.0, 34.0));

    let (x0, x1) = range(series, |p| p.0, x_axis.log);
    let (y0, y1) = range(series, |p| p.1, false);
    let fx = |x: f64| -> f32 {
        let t = if x_axis.log {
            (x.log10() - x0.log10()) / (x1.log10() - x0.log10())
        } else {
            (x - x0) / (x1 - x0)
        };
        area.left() + t as f32 * area.width()
    };
    let fy = |y: f64| -> f32 { area.bottom() - ((y - y0) / (y1 - y0)) as f32 * area.height() };
    let inv_x = |px: f32| -> f64 {
        let t = ((px - area.left()) / area.width()) as f64;
        if x_axis.log {
            10f64.powf(x0.log10() + t * (x1.log10() - x0.log10()))
        } else {
            x0 + t * (x1 - x0)
        }
    };

    let font = FontId::proportional(11.0);
    let x_ticks = if x_axis.log {
        log_ticks(x0, x1)
    } else {
        linear_ticks(x0, x1, 8)
    };
    for x in x_ticks {
        let px = fx(x);
        painter.line_segment(
            [pos2(px, area.top()), pos2(px, area.bottom())],
            Stroke::new(1.0_f32, grid),
        );
        painter.text(
            pos2(px, area.bottom() + 3.0),
            Align2::CENTER_TOP,
            format_value(x, x_axis.unit),
            font.clone(),
            text,
        );
    }
    if x_axis.log {
        // Minor lines at 2..9 within each decade.
        let mut d = 10f64.powf(x0.log10().floor());
        while d < x1 {
            for m in 2..10 {
                let x = d * m as f64;
                if x > x0 && x < x1 {
                    let px = fx(x);
                    painter.line_segment(
                        [pos2(px, area.top()), pos2(px, area.bottom())],
                        Stroke::new(1.0_f32, grid.gamma_multiply(0.45)),
                    );
                }
            }
            d *= 10.0;
        }
    }
    for y in linear_ticks(y0, y1, 6) {
        let py = fy(y);
        let strong = y.abs() < 1e-12 * (y1 - y0).abs().max(1e-30);
        let color = if strong {
            text.gamma_multiply(0.6)
        } else {
            grid
        };
        painter.line_segment(
            [pos2(area.left(), py), pos2(area.right(), py)],
            Stroke::new(1.0_f32, color),
        );
        painter.text(
            pos2(area.left() - 4.0, py),
            Align2::RIGHT_CENTER,
            format_value(y, y_axis.unit),
            font.clone(),
            text,
        );
    }
    painter.text(
        pos2(area.center().x, rect.bottom() - 4.0),
        Align2::CENTER_BOTTOM,
        x_axis.label,
        font.clone(),
        text,
    );
    painter.text(
        rect.left_top() + vec2(4.0, 2.0),
        Align2::LEFT_TOP,
        y_axis.label,
        font.clone(),
        text,
    );

    let clip = painter.with_clip_rect(area.expand(1.0));
    for s in series {
        let pts: Vec<Pos2> = s
            .points
            .iter()
            .filter(|(x, y)| x.is_finite() && y.is_finite() && (!x_axis.log || *x > 0.0))
            .map(|&(x, y)| pos2(fx(x), fy(y)))
            .collect();
        if pts.len() >= 2 {
            clip.add(Shape::line(pts, Stroke::new(1.8_f32, s.color)));
        }
    }

    // Legend.
    let mut ly = area.top() + 4.0;
    for s in series.iter().filter(|s| !s.label.is_empty()) {
        painter.text(
            pos2(area.right() - 6.0, ly),
            Align2::RIGHT_TOP,
            &s.label,
            font.clone(),
            s.color,
        );
        ly += 14.0;
    }

    // Hover cursor.
    let hover = response.hover_pos().filter(|p| area.contains(*p))?;
    let x = inv_x(hover.x);
    painter.line_segment(
        [pos2(hover.x, area.top()), pos2(hover.x, area.bottom())],
        Stroke::new(1.0_f32, text.gamma_multiply(0.5)),
    );
    let mut lines = vec![format!(
        "{} = {}",
        x_axis.label,
        format_value(x, x_axis.unit)
    )];
    for s in series {
        if let Some(y) = interpolate(&s.points, x) {
            painter.circle_filled(pos2(hover.x, fy(y)), 3.0, s.color);
            let name = if s.label.is_empty() {
                y_axis.label
            } else {
                &s.label
            };
            lines.push(format!("{name}: {}", format_value(y, y_axis.unit)));
        }
    }
    let galley = painter.layout_no_wrap(lines.join("\n"), FontId::monospace(11.0), text);
    let mut at = hover + vec2(10.0, 10.0);
    if at.x + galley.size().x > area.right() {
        at.x = hover.x - 10.0 - galley.size().x;
    }
    painter.rect_filled(
        Rect::from_min_size(at, galley.size()).expand(3.0),
        3.0,
        bg.gamma_multiply(0.92),
    );
    painter.galley(at, galley, text);
    Some(x)
}

/// Linear interpolation of `points` (sorted by x) at `x`.
pub fn interpolate(points: &[(f64, f64)], x: f64) -> Option<f64> {
    let i = points.partition_point(|p| p.0 < x);
    if i == 0 || i >= points.len() {
        return None;
    }
    let ((x0, y0), (x1, y1)) = (points[i - 1], points[i]);
    Some(if x1 > x0 {
        y0 + (y1 - y0) * (x - x0) / (x1 - x0)
    } else {
        y1
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks() {
        assert_eq!(
            linear_ticks(0.0, 10.0, 5),
            vec![0.0, 2.0, 4.0, 6.0, 8.0, 10.0]
        );
        // Steps of 0.1 from -0.3 to 0.3, including (essentially) zero.
        let t = linear_ticks(-0.3, 0.3, 6);
        assert_eq!(t.len(), 7);
        assert!(t.windows(2).all(|w| (w[1] - w[0] - 0.1).abs() < 1e-12));
        assert!(t.iter().any(|v| v.abs() < 1e-12));
        assert_eq!(log_ticks(5.0, 2e4), vec![10.0, 100.0, 1e3, 1e4]);
        assert_eq!(interpolate(&[(0.0, 0.0), (2.0, 4.0)], 1.0), Some(2.0));
        assert_eq!(interpolate(&[(0.0, 0.0), (2.0, 4.0)], 3.0), None);
    }
}
