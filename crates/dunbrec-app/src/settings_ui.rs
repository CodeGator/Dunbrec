//! The Settings dialog: appearance, and which language model the Assistant
//! talks to (Ollama or Claude).
//!
//! Choices apply as they are made and are saved with the other settings. The
//! Claude API key is the exception: it goes to the Windows Credential
//! Manager, never to the settings file.

use std::sync::mpsc::{Receiver, channel};

use egui::{Color32, ComboBox, Context, RichText, TextEdit, Ui};

use crate::ai::{self, AiSettings, Provider};
use crate::settings::{Settings, TextSize, Theme};

type ModelList = Result<Vec<String>, String>;

/// The pages of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Category {
    #[default]
    Appearance,
    Assistant,
}

impl Category {
    const ALL: [Category; 2] = [Category::Appearance, Category::Assistant];

    fn name(self) -> &'static str {
        match self {
            Category::Appearance => "Appearance",
            Category::Assistant => "Assistant",
        }
    }
}

#[derive(Default)]
pub struct SettingsDialog {
    pub open: bool,
    category: Category,
    key_input: String,
    key_saved: bool,
    /// Models the last successful test found.
    models: Vec<String>,
    /// A test in progress.
    testing: Option<Receiver<ModelList>>,
    /// Outcome of the last key change or test: (worked, text).
    status: Option<(bool, String)>,
    /// Fetch the model list on the next frame.
    refresh: bool,
}

impl SettingsDialog {
    pub fn open(&mut self) {
        self.open = true;
        self.key_saved = ai::key::exists();
        self.status = None;
        self.models.clear();
        self.refresh = true;
    }

    pub fn show(&mut self, ctx: &Context, settings: &mut Settings) {
        self.poll(&mut settings.ai);
        if !self.open {
            return;
        }
        if self.refresh && self.testing.is_none() {
            self.refresh = false;
            let ai = &settings.ai;
            if ai.provider == Provider::Ollama || self.key_saved {
                self.test(ai, ctx);
            }
        }
        let mut open = true;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    // The categories, one page each.
                    ui.vertical(|ui| {
                        ui.set_width(110.0);
                        for category in Category::ALL {
                            ui.selectable_value(&mut self.category, category, category.name());
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width(440.0);
                        match self.category {
                            Category::Appearance => self.appearance(ui, ctx, settings),
                            Category::Assistant => self.assistant(ui, ctx, &mut settings.ai),
                        }
                    });
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
        }
        self.open = open;
    }

    fn appearance(&mut self, ui: &mut Ui, ctx: &Context, settings: &mut Settings) {
        ui.heading("Appearance");
        ui.horizontal(|ui| {
            ui.label("Theme");
            for (theme, name) in [
                (Theme::System, "Follow Windows"),
                (Theme::Light, "Light"),
                (Theme::Dark, "Dark"),
            ] {
                if ui.radio_value(&mut settings.theme, theme, name).clicked() {
                    ctx.set_theme(theme.preference());
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Text size");
            for size in TextSize::ALL {
                if ui
                    .radio_value(&mut settings.text_size, size, size.name())
                    .clicked()
                {
                    ctx.set_zoom_factor(size.factor());
                }
            }
        });
        ui.label(
            RichText::new("Makes everything larger: menus, panels, dialogs and the schematic.")
                .weak(),
        );
    }

    fn assistant(&mut self, ui: &mut Ui, ctx: &Context, ai: &mut AiSettings) {
        ui.heading("Assistant");
        ui.label(
            RichText::new(
                "The language model that answers in the Assistant panel. \
                 Ollama runs on your own computer; Claude is Anthropic's service, \
                 and the open circuit and firmware are sent to it.",
            )
            .weak(),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Provider");
            for provider in [Provider::Ollama, Provider::Claude] {
                if ui
                    .radio_value(&mut ai.provider, provider, provider.name())
                    .clicked()
                {
                    self.models.clear();
                    self.status = None;
                    self.refresh = true;
                }
            }
        });
        ui.add_space(4.0);

        egui::Grid::new("assistant_grid")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| match ai.provider {
                Provider::Ollama => {
                    ui.label("Server address");
                    ui.add(TextEdit::singleline(&mut ai.ollama_url).desired_width(300.0));
                    ui.end_row();
                    ui.label("Model");
                    model_row(ui, "ollama_model", &mut ai.ollama_model, &self.models);
                    ui.end_row();
                    ui.label("Context size");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::DragValue::new(&mut ai.ollama_context)
                                .range(2048..=131_072)
                                .speed(256),
                        );
                        ui.label(RichText::new("tokens").weak());
                        for size in [8192, 16_384, 32_768] {
                            if ui.small_button(format!("{}k", size / 1024)).clicked() {
                                ai.ollama_context = size;
                            }
                        }
                    });
                    ui.end_row();
                }
                Provider::Claude => {
                    ui.label("API key");
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut self.key_input)
                                .password(true)
                                .hint_text(if self.key_saved {
                                    "saved — paste a new key to replace it"
                                } else {
                                    "sk-ant-…"
                                })
                                .desired_width(220.0),
                        );
                        if ui
                            .add_enabled(
                                !self.key_input.trim().is_empty(),
                                egui::Button::new("Save"),
                            )
                            .clicked()
                        {
                            self.save_key();
                        }
                        if ui
                            .add_enabled(self.key_saved, egui::Button::new("Remove"))
                            .clicked()
                        {
                            self.remove_key();
                        }
                    });
                    ui.end_row();
                    ui.label("Model");
                    model_row(ui, "claude_model", &mut ai.claude_model, &self.models);
                    ui.end_row();
                }
            });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let busy = self.testing.is_some();
            let can = !busy && (ai.provider == Provider::Ollama || self.key_saved);
            if ui
                .add_enabled(can, egui::Button::new("Test connection"))
                .clicked()
            {
                self.test(ai, ctx);
            }
            if busy {
                ui.spinner();
            }
            if let Some((ok, text)) = &self.status {
                let color = if *ok {
                    Color32::from_rgb(90, 170, 110)
                } else {
                    Color32::from_rgb(210, 65, 65)
                };
                ui.add(egui::Label::new(RichText::new(text).color(color)).wrap());
            }
        });
        if ai.provider == Provider::Ollama {
            ui.label(
                RichText::new(
                    "Context size is how much text the model reads at once. The circuit \n                     and firmware need several thousand tokens; larger values use more \n                     video memory and run slower.",
                )
                .weak(),
            );
        }
        if ai.provider == Provider::Claude {
            ui.label(RichText::new("The key is kept in the Windows Credential Manager.").weak());
        }
    }

    fn save_key(&mut self) {
        match ai::key::set(&self.key_input) {
            Ok(()) => {
                self.key_input.clear();
                self.key_saved = true;
                self.refresh = true;
                self.status = Some((true, "Key saved.".into()));
            }
            Err(e) => self.status = Some((false, format!("Could not save the key: {e}"))),
        }
    }

    fn remove_key(&mut self) {
        match ai::key::remove() {
            Ok(()) => {
                self.key_saved = false;
                self.status = Some((true, "Key removed.".into()));
            }
            Err(e) => self.status = Some((false, format!("Could not remove the key: {e}"))),
        }
    }

    /// Ask the provider for its models: proves the address or key works and
    /// fills the model list.
    fn test(&mut self, ai: &AiSettings, ctx: &Context) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let provider = ai.provider;
        let url = ai.ollama_url.clone();
        std::thread::spawn(move || {
            let result = match provider {
                Provider::Ollama => ai::ollama_models(&url),
                Provider::Claude => ai::claude_models(),
            };
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.testing = Some(rx);
        self.status = None;
    }

    fn poll(&mut self, ai: &mut AiSettings) {
        let Some(rx) = &self.testing else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.testing = None;
        match result {
            Ok(models) => {
                self.status = Some((
                    true,
                    format!(
                        "Connected: {} model{} available.",
                        models.len(),
                        if models.len() == 1 { "" } else { "s" }
                    ),
                ));
                if ai.provider == Provider::Ollama
                    && ai.ollama_model.trim().is_empty()
                    && let Some(first) = models.first()
                {
                    ai.ollama_model = first.clone();
                }
                self.models = models;
            }
            Err(e) => self.status = Some((false, e)),
        }
    }
}

/// A model name that can be typed, with a menu of the models found by a test.
fn model_row(ui: &mut Ui, id: &str, model: &mut String, found: &[String]) {
    ui.horizontal(|ui| {
        ui.add(TextEdit::singleline(model).desired_width(220.0));
        let none = found.is_empty();
        let picker = ui.add_enabled_ui(!none, |ui| {
            ComboBox::from_id_salt(id)
                .selected_text("Choose")
                .width(70.0)
                .show_ui(ui, |ui| {
                    for name in found {
                        ui.selectable_value(model, name.clone(), name);
                    }
                });
        });
        if none {
            picker
                .response
                .on_disabled_hover_text("Press Test connection to list the available models.");
        }
    });
}
