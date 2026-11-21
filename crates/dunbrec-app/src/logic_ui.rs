//! Logic analyser: eight channels showing nets as logic levels over time.
//! Levels use the same thresholds and hysteresis as the logic parts' inputs.

// Channels are addressed by number (levels, edges and names are parallel
// arrays), so index loops read most clearly.
#![allow(clippy::needless_range_loop)]

use std::collections::VecDeque;

use dunbrec_core::units::format_value;
use dunbrec_sim::digital::CMOS_5V;
use egui::{Align2, Color32, FontId, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};

pub const LOGIC_CHANNELS: usize = 8;
/// Screen width in divisions.
const DIVISIONS: f64 = 10.0;
/// Transitions older than this many screen widths are discarded.
const HISTORY: f64 = 4.0;

pub const LOGIC_COLOR: Color32 = Color32::from_rgb(120, 230, 160);

#[derive(Debug, Clone)]
pub struct LogicAnalyzer {
    pub time_per_div: f64,
    /// Present level of each channel (`None`: no signal yet).
    level: [Option<bool>; LOGIC_CHANNELS],
    /// Level changes per channel: (time, new level).
    edges: [VecDeque<(f64, bool)>; LOGIC_CHANNELS],
    last_time: f64,
}

impl Default for LogicAnalyzer {
    fn default() -> Self {
        Self {
            time_per_div: 1e-3,
            level: [None; LOGIC_CHANNELS],
            edges: Default::default(),
            last_time: 0.0,
        }
    }
}

impl LogicAnalyzer {
    pub fn window(&self) -> f64 {
        self.time_per_div * DIVISIONS
    }

    pub fn clear(&mut self) {
        let tpd = self.time_per_div;
        *self = Self::default();
        self.time_per_div = tpd;
    }

    /// Record one sample (NaN for channels without a probe).
    pub fn push(&mut self, t: f64, volts: [f64; LOGIC_CHANNELS]) {
        if t < self.last_time {
            self.clear();
        }
        self.last_time = t;
        let keep_after = t - HISTORY * self.window();
        for (ch, v) in volts.into_iter().enumerate() {
            if !v.is_finite() {
                self.level[ch] = None;
                self.edges[ch].clear();
                continue;
            }
            let new = match self.level[ch] {
                None => v >= 0.5 * (CMOS_5V.vil + CMOS_5V.vih),
                Some(true) => v > CMOS_5V.vil,
                Some(false) => v >= CMOS_5V.vih,
            };
            if self.level[ch] != Some(new) {
                self.edges[ch].push_back((t, new));
                self.level[ch] = Some(new);
            }
            // Keep the last change before the window so its level is known.
            while self.edges[ch].len() > 1 && self.edges[ch][1].0 < keep_after {
                self.edges[ch].pop_front();
            }
        }
    }

    /// Level of channel `ch` at time `t`, if known.
    pub fn level_at(&self, ch: usize, t: f64) -> Option<bool> {
        let e = &self.edges[ch];
        let i = e.partition_point(|(et, _)| *et <= t);
        (i > 0).then(|| e[i - 1].1)
    }

    /// Number of rising edges of channel `ch` within the window.
    fn rising_edges(&self, ch: usize, from: f64) -> usize {
        self.edges[ch]
            .iter()
            .filter(|(t, l)| *l && *t >= from)
            .count()
    }

    /// Draw the traces; `names[ch]` labels each row.
    pub fn draw(&self, ui: &mut Ui, names: &[String; LOGIC_CHANNELS]) {
        let row_h = 26.0;
        let label_w = 90.0;
        let width = ui.available_width().max(300.0);
        let (rect, _) = ui.allocate_exact_size(
            vec2(width, row_h * LOGIC_CHANNELS as f32 + 18.0),
            Sense::hover(),
        );
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, Color32::from_rgb(8, 14, 12));
        let plot = Rect::from_min_max(rect.min + vec2(label_w, 4.0), rect.max - vec2(8.0, 18.0));
        let grid = Color32::from_rgb(40, 60, 52);
        for i in 0..=DIVISIONS as i32 {
            let x = plot.left() + plot.width() * i as f32 / DIVISIONS as f32;
            painter.line_segment(
                [pos2(x, plot.top()), pos2(x, plot.bottom())],
                Stroke::new(1.0_f32, grid),
            );
        }
        let end = self.last_time;
        let start = end - self.window();
        let x_of = |t: f64| plot.left() + ((t - start) / self.window()) as f32 * plot.width();
        for ch in 0..LOGIC_CHANNELS {
            let top = plot.top() + ch as f32 * row_h;
            let (hi, lo) = (top + 5.0, top + row_h - 5.0);
            painter.text(
                pos2(rect.left() + 6.0, (hi + lo) / 2.0),
                Align2::LEFT_CENTER,
                format!("D{ch} {}", names[ch]),
                FontId::monospace(11.0),
                if self.level[ch].is_some() {
                    LOGIC_COLOR
                } else {
                    Color32::GRAY
                },
            );
            let Some(initial) = self
                .level_at(ch, start)
                .or(self.edges[ch].front().map(|e| !e.1))
            else {
                continue;
            };
            let y = |l: bool| if l { hi } else { lo };
            let mut pts = vec![pos2(plot.left(), y(initial))];
            let mut level = initial;
            for &(t, l) in self.edges[ch].iter().filter(|(t, _)| *t > start) {
                let x = x_of(t);
                pts.push(pos2(x, y(level)));
                pts.push(pos2(x, y(l)));
                level = l;
            }
            pts.push(pos2(plot.right(), y(level)));
            painter.add(Shape::line(pts, Stroke::new(1.6_f32, LOGIC_COLOR)));
            // Frequency read-out from rising edges in view.
            let n = self.rising_edges(ch, start);
            if n >= 2 {
                let rises: Vec<f64> = self.edges[ch]
                    .iter()
                    .filter(|(t, l)| *l && *t >= start)
                    .map(|e| e.0)
                    .collect();
                let f = (rises.len() - 1) as f64 / (rises[rises.len() - 1] - rises[0]);
                painter.text(
                    pos2(plot.right() - 4.0, hi),
                    Align2::RIGHT_TOP,
                    format_value(f, "Hz"),
                    FontId::monospace(10.0),
                    Color32::from_rgb(170, 200, 185),
                );
            }
        }
        painter.text(
            pos2(plot.right(), rect.bottom() - 3.0),
            Align2::RIGHT_BOTTOM,
            format!("{}/div", format_value(self.time_per_div, "s")),
            FontId::monospace(11.0),
            Color32::from_rgb(200, 220, 210),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hysteresis_and_history() {
        let mut la = LogicAnalyzer::default();
        let mut v = [f64::NAN; LOGIC_CHANNELS];
        // A slow ramp 0 → 5 → 0: switches high at 3.5 V and low at 1.5 V.
        for k in 0..=100 {
            let t = k as f64 * 1e-4;
            // Offset by 0.05 V so no sample sits exactly on a threshold.
            let volts = if k <= 50 {
                k as f64 * 0.1
            } else {
                (100 - k) as f64 * 0.1
            } + 0.05;
            v[0] = volts;
            la.push(t, v);
        }
        let edges: Vec<(f64, bool)> = la.edges[0].iter().copied().collect();
        assert_eq!(edges.len(), 3, "{edges:?}");
        assert!(!edges[0].1);
        assert!((edges[1].0 - 35e-4).abs() < 1e-12 && edges[1].1);
        assert!((edges[2].0 - 86e-4).abs() < 1e-12 && !edges[2].1);
        assert_eq!(la.level_at(0, 5e-3), Some(true));
        assert!(la.level[1].is_none());
        // Time going backwards (a restart) clears everything.
        la.push(0.0, v);
        assert_eq!(la.edges[0].len(), 1);
    }
}
