//! Serial monitor: text a microcontroller sends on its serial port
//! (`Serial.print`), one log per microcontroller.

use std::collections::BTreeMap;

use egui::{Color32, FontId, RichText};

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

    #[cfg(test)]
    pub fn text(&self, designator: &str) -> Option<&str> {
        self.logs.get(designator).map(|s| s.as_str())
    }

    /// The user closed the monitor: stay closed until they open it again.
    pub fn close(&mut self) {
        self.show = false;
        self.dismissed = true;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
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
                            ui.label(
                                RichText::new(text)
                                    .font(FontId::monospace(12.0))
                                    .color(Color32::from_rgb(200, 230, 210)),
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
