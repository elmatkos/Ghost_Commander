//! Theme list, live editor window and persistence of the selected theme.

use eframe::egui::{self, Key};
use gc_core::filetype::Category;

use crate::theme::{
    extensions_from_text, extensions_to_text, load_custom_themes, load_settings, resolve_icon, save_custom_theme, save_settings,
    Theme,
};

pub struct ThemeManager {
    themes: Vec<Theme>,
    /// Index into `themes` of the selected theme.
    selected: usize,
    /// Working copy: what is shown and edited. Differs from `themes[selected]` after edits.
    pub active: Theme,
    pub open: bool,
    extensions_text: String,
    save_name: String,
    message: Option<String>,
}

impl ThemeManager {
    /// Loads built-in and custom themes and the remembered selection.
    pub fn load(ctx: &egui::Context) -> Self {
        let mut themes = Theme::builtins();
        let (custom, problems) = load_custom_themes();
        themes.extend(custom);
        let wanted = load_settings().theme;
        let selected = wanted.and_then(|w| themes.iter().position(|t| t.name == w)).unwrap_or(0);
        let active = themes[selected].clone();
        active.apply(ctx);
        Self {
            extensions_text: extensions_to_text(&active.extensions),
            save_name: active.name.clone(),
            themes,
            selected,
            active,
            open: false,
            message: (!problems.is_empty()).then(|| problems.join("\n")),
        }
    }

    fn select(&mut self, ctx: &egui::Context, index: usize) {
        self.selected = index;
        self.active = self.themes[index].clone();
        self.extensions_text = extensions_to_text(&self.active.extensions);
        self.save_name = self.active.name.clone();
        self.active.apply(ctx);
        let mut settings = load_settings();
        settings.theme = Some(self.active.name.clone());
        self.message = save_settings(&settings).err().map(|e| format!("Cannot remember theme: {e}"));
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.open = false;
            return;
        }
        let mut open = self.open;
        let mut changed = false;
        let mut pick: Option<usize> = None;
        let mut save = false;

        egui::Window::new("Themes").open(&mut open).collapsible(false).default_width(380.0).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Theme");
                egui::ComboBox::from_id_salt("theme_pick").selected_text(&self.themes[self.selected].name).show_ui(ui, |ui| {
                    for (i, t) in self.themes.iter().enumerate() {
                        if ui.selectable_label(i == self.selected, &t.name).clicked() {
                            pick = Some(i);
                        }
                    }
                });
            });
            ui.separator();

            ui.horizontal(|ui| {
                changed |= ui.radio_value(&mut self.active.dark, true, "Dark base").changed();
                changed |= ui.radio_value(&mut self.active.dark, false, "Light base").changed();
            });
            egui::Grid::new("theme_colors").num_columns(2).show(ui, |ui| {
                let t = &mut self.active;
                for (label, color) in [
                    ("Background", &mut t.background),
                    ("Text", &mut t.text),
                    ("Cursor / selection", &mut t.accent),
                    ("Folders", &mut t.dir),
                    ("Marked", &mut t.marked),
                    ("Errors", &mut t.error),
                ] {
                    ui.label(label);
                    changed |= ui.color_edit_button_srgba(&mut color.0).changed();
                    ui.end_row();
                }
            });

            ui.collapsing("File type icons and colors", |ui| {
                ui.label("Icon: a name (folder, archive, image, star, ...) or any single character.");
                egui::Grid::new("theme_types").num_columns(4).show(ui, |ui| {
                    for cat in Category::ALL {
                        let Some(style) = self.active.types.get_mut(cat.name()) else { continue };
                        ui.label(cat.name());
                        changed |= ui.color_edit_button_srgba(&mut style.color.0).changed();
                        changed |= ui.add(egui::TextEdit::singleline(&mut style.icon).desired_width(90.0)).changed();
                        ui.label(egui::RichText::new(resolve_icon(&style.icon)).color(style.color.0).size(18.0));
                        ui.end_row();
                    }
                });
            });

            ui.collapsing("Extensions", |ui| {
                ui.label("One per line: extension=category (e.g. mkv=video). Overrides the built-in mapping.");
                if ui.add(egui::TextEdit::multiline(&mut self.extensions_text).desired_rows(4).desired_width(f32::INFINITY)).changed() {
                    self.active.extensions = extensions_from_text(&self.extensions_text);
                    changed = true;
                }
            });

            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Save as");
                ui.add(egui::TextEdit::singleline(&mut self.save_name).desired_width(160.0));
                save = ui.button("Save custom theme").clicked();
            });
            if let Some(m) = &self.message {
                let color = if m.starts_with("Saved") { self.active.text.0 } else { self.active.error.0 };
                ui.colored_label(color, m);
            }
        });

        self.open = open;
        if let Some(i) = pick {
            self.select(ctx, i);
        } else if changed {
            self.active.apply(ctx);
        }
        if save {
            self.save_active(ctx);
        }
    }

    fn save_active(&mut self, ctx: &egui::Context) {
        let mut theme = self.active.clone();
        theme.name = self.save_name.trim().to_owned();
        match save_custom_theme(&theme) {
            Ok(path) => {
                match self.themes.iter().position(|t| t.name == theme.name) {
                    Some(i) => self.themes[i] = theme,
                    None => self.themes.push(theme),
                }
                let index = self.themes.iter().position(|t| t.name == self.save_name.trim()).unwrap_or(0);
                self.select(ctx, index);
                self.message = Some(format!("Saved to {}", path.display()));
            }
            Err(e) => self.message = Some(format!("Cannot save: {e}")),
        }
    }
}
