//! Serial monitor: text a microcontroller sends on its serial port
//! (`Serial.print`), one log per microcontroller.

use crate::float::{self, Spec};
use std::collections::BTreeMap;

use egui::{RichText, TextStyle};

/// Most characters kept per microcontroller.
const MAX_CHARS: usize = 64 * 1024;

#[derive(Debug, Default)]
pub struct SerialMonitor {
    pub show: bool,
    /// Closed by the user during this session: don't pop up again.
    dismissed: bool,
    /// Text received, by designator.
    logs: BTreeMap<String, String>,
    selected: Option<String>,
}

impl SerialMonitor {
    pub fn clear(&mut self) {
        self.logs.clear();
    }

    /// Append bytes from microcontroller `designator`, opening the window
    /// the first time anything arrives (unless the user closed it).
    pub fn push(&mut self, designator: &str, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let log = self.logs.entry(designator.to_string()).or_default();
        for &b in bytes {
            match b {
                b'\n' => log.push('\n'),
                b'\r' => {}
                0x20..=0x7E => log.push(b as char),
                b'\t' => log.push('\t'),
                _ => log.push_str(&format!("\\x{b:02X}")),
            }
        }
        if log.len() > MAX_CHARS {
            let cut = log.len() - MAX_CHARS;
            let cut = (cut..log.len())
                .find(|&i| log.is_char_boundary(i))
                .unwrap_or(log.len());
            log.drain(..cut);
        }
        if self.selected.is_none() {
            self.selected = Some(designator.to_string());
        }
        if !self.show && !self.dismissed {
            self.show = true;
        }
    }

    /// The last `max` characters of each microcontroller's output.
    pub fn tails(&self, max: usize) -> Vec<(String, String)> {
        self.logs
            .iter()
            .filter(|(_, log)| !log.is_empty())
            .map(|(name, log)| {
                let start = (log.len().saturating_sub(max)..=log.len())
                    .find(|&i| log.is_char_boundary(i))
                    .unwrap_or(log.len());
                (name.clone(), log[start..].to_string())
            })
            .collect()
    }

    #[cfg(test)]
    pub fn text(&self, designator: &str) -> Option<&str> {
        self.logs.get(designator).map(|s| s.as_str())
    }

    /// The monitor as a floating window; closing it keeps it closed until
    /// the user opens it again.
    pub fn window(&mut self, ctx: &egui::Context) {
        if !self.show {
            return;
        }
        let mut open = true;
        let spec = Spec {
            title: float::SERIAL_MONITOR,
            size: [460.0, 300.0],
            resizable: true,
            pos: None,
        };
        float::window(ctx, spec, &mut open, |ui| self.ui(ui));
        if !open {
            self.show = false;
            self.dismissed = true;
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        {
            {
                ui.horizontal(|ui| {
                    if self.logs.len() > 1 {
                        let current = self.selected.clone().unwrap_or_default();
                        egui::ComboBox::from_id_salt("serial_mcu")
                            .selected_text(&current)
                            .show_ui(ui, |ui| {
                                for name in self.logs.keys() {
                                    if ui.selectable_label(*name == current, name).clicked() {
                                        self.selected = Some(name.clone());
                                    }
                                }
                            });
                    } else if let Some(name) = &self.selected {
                        ui.label(RichText::new(name).strong());
                    }
                    if ui.button("Clear").clicked()
                        && let Some(name) = &self.selected
                    {
                        self.logs.remove(name);
                    }
                    ui.label(RichText::new("TX of each microcontroller (pin D1)").weak());
                });
                ui.separator();
                let text = self
                    .selected
                    .as_ref()
                    .and_then(|n| self.logs.get(n))
                    .map(|s| s.as_str())
                    .unwrap_or("");
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if text.is_empty() {
                            ui.label(
                                RichText::new(
                                    "Nothing received yet. Run a circuit whose \
                                     microcontroller writes to its serial port.",
                                )
                                .weak(),
                            );
                        } else {
                            // The theme's strongest text, to be easy to read
                            // on its background, light or dark.
                            let colour = ui.visuals().strong_text_color();
                            ui.label(
                                RichText::new(text)
                                    .text_style(TextStyle::Monospace)
                                    .color(colour),
                            );
                        }
                    });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_arrives_stands_out_in_both_themes() {
        let brightness = |c: egui::Color32| {
            (0.2126 * f32::from(c.r()) + 0.7152 * f32::from(c.g()) + 0.0722 * f32::from(c.b()))
                / 255.0
        };
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            let mut m = SerialMonitor::default();
            m.push("U1", b"Reading = 511");
            let ctx = egui::Context::default();
            ctx.set_theme(theme);
            let mut behind = egui::Color32::TRANSPARENT;
            let mut out = Default::default();
            for _ in 0..2 {
                out = ctx.run(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        behind = ui.visuals().panel_fill;
                        m.ui(ui);
                    });
                });
            }
            let out: egui::FullOutput = out;
            let ink = out
                .shapes
                .iter()
                .find_map(|c| match &c.shape {
                    egui::Shape::Text(t) if t.galley.text() == "Reading = 511" => {
                        Some(t.galley.job.sections[0].format.color)
                    }
                    _ => None,
                })
                .expect("the text is drawn");
            let apart = (brightness(ink) - brightness(behind)).abs();
            assert!(apart > 0.6, "{theme:?}: {ink:?} on {behind:?}");
        }
    }

    #[test]
    fn decoding_and_limits() {
        let mut m = SerialMonitor::default();
        m.push("U1", b"Hello\r\nA0 = 0512\r\n\x01");
        assert_eq!(m.text("U1"), Some("Hello\nA0 = 0512\n\\x01"));
        assert!(m.show, "opens on first data");
        m.show = false;
        m.dismissed = true;
        m.push("U1", b"x");
        assert!(!m.show, "stays closed once dismissed");
        let big = vec![b'a'; MAX_CHARS + 10];
        m.push("U2", &big);
        assert_eq!(m.text("U2").unwrap().len(), MAX_CHARS);
    }
}
