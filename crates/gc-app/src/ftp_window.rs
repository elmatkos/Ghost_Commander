//! Net > FTP connect (Ctrl+N): quick connect from a link and saved sessions.

use eframe::egui::{self, Key};
use gc_ftp::sessions::{self, Session};
use gc_ftp::Target;

use crate::theme::config_dir;

#[derive(Default)]
pub struct FtpWindow {
    pub open: bool,
    loaded: bool,
    sessions: Vec<Session>,
    sel: Option<usize>,
    url: String,
    focus_url: bool,
    message: Option<String>,
}

impl FtpWindow {
    pub fn open(&mut self) {
        if !self.loaded {
            self.sessions = config_dir().map(|d| sessions::load(&d)).unwrap_or_default();
            self.loaded = true;
        }
        self.open = true;
        self.focus_url = true;
        self.message = None;
        // Start with the scheme typed; the user completes or overwrites it.
        if self.url.trim().is_empty() {
            self.url = "ftp://".into();
        }
    }

    fn save(&mut self) {
        self.message = match config_dir() {
            Some(dir) => match sessions::save(&dir, &self.sessions) {
                Ok(warning) => warning,
                Err(e) => Some(format!("Cannot save sessions: {e}")),
            },
            None => Some("APPDATA is not set, sessions cannot be saved.".into()),
        };
    }

    /// A link without a login (`ftp://host/dir`) uses the saved session of that host when there is one.
    fn target_from_url(&self) -> Result<Target, String> {
        let mut t = Target::from_url(&self.url)?;
        if !self.url.contains('@') {
            if let Some(s) = self.sessions.iter().find(|s| s.host.eq_ignore_ascii_case(&t.host) && s.port == t.port) {
                let mut saved = s.target();
                if t.path != "/" {
                    saved.path = t.path;
                }
                t = saved;
                t.secure |= self.url.trim_start().to_ascii_lowercase().starts_with("ftps://");
            }
        }
        Ok(t)
    }

    /// Draws the window. Returns the server to connect the active panel to, when the user asks for it.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<Target> {
        if !self.open {
            return None;
        }
        let mut open = self.open;
        let mut connect: Option<Target> = None;
        let mut save_url_session = false;
        let mut save_now = false;
        // Never taller than the app window, so the title bar stays reachable; the content scrolls.
        let max_h = (ctx.screen_rect().height() - 90.0).max(200.0);
        egui::Window::new("FTP connect").open(&mut open).collapsible(false).resizable([true, false]).default_width(560.0).max_height(max_h).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            // Text fields get the outline that normally shows only under the cursor.
            let outline = ui.visuals().widgets.hovered.bg_stroke;
            ui.visuals_mut().widgets.inactive.bg_stroke = outline;
            egui::ScrollArea::vertical().max_height(max_h - 50.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.label("Link:  ftp://user:password@host:port/folder   (ftps:// = TLS)");
            ui.weak("Special characters in the login: %40 for @, %3A for :, %2F for /");
            ui.horizontal(|ui| {
                let edit = ui.add(egui::TextEdit::singleline(&mut self.url).desired_width(380.0));
                if std::mem::take(&mut self.focus_url) {
                    edit.request_focus();
                    // Cursor after "ftp://" so typing continues the address.
                    if let Some(mut state) = egui::TextEdit::load_state(ctx, edit.id) {
                        let end = egui::text::CCursor::new(self.url.chars().count());
                        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
                        state.store(ctx, edit.id);
                    }
                }
                let enter = edit.lost_focus() && ctx.input(|i| i.key_pressed(Key::Enter));
                if ui.button("Connect").clicked() || enter {
                    match self.target_from_url() {
                        Ok(t) => connect = Some(t),
                        Err(e) => self.message = Some(e),
                    }
                }
                if ui.button("Save as session").clicked() {
                    save_url_session = true;
                }
            });
            ui.separator();
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(170.0);
                    ui.strong("Saved sessions");
                    egui::ScrollArea::vertical().max_height(220.0).id_salt("ftp_sessions").show(ui, |ui| {
                        for (i, s) in self.sessions.iter().enumerate() {
                            if ui.selectable_label(self.sel == Some(i), &s.name).clicked() {
                                self.sel = Some(i);
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.button("New").clicked() {
                            self.sessions.push(Session::new());
                            self.sel = Some(self.sessions.len() - 1);
                        }
                        if ui.add_enabled(self.sel.is_some(), egui::Button::new("Delete")).clicked() {
                            if let Some(i) = self.sel.take() {
                                self.sessions.remove(i);
                                save_now = true;
                            }
                        }
                    });
                });
                ui.separator();
                ui.vertical(|ui| {
                    let Some(s) = self.sel.and_then(|i| self.sessions.get_mut(i)) else {
                        ui.weak("Pick a session, or press New.");
                        return;
                    };
                    egui::Grid::new("ftp_session_form").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                        ui.label("Name");
                        ui.text_edit_singleline(&mut s.name);
                        ui.end_row();
                        ui.label("Host");
                        ui.text_edit_singleline(&mut s.host);
                        ui.end_row();
                        ui.label("Port");
                        ui.add(egui::DragValue::new(&mut s.port).range(1..=65535));
                        ui.end_row();
                        ui.label("User");
                        ui.text_edit_singleline(&mut s.user);
                        ui.end_row();
                        ui.label("Password");
                        ui.add(egui::TextEdit::singleline(&mut s.password).password(true));
                        ui.end_row();
                        ui.label("Folder");
                        ui.text_edit_singleline(&mut s.path);
                        ui.end_row();
                    });
                    ui.checkbox(&mut s.secure, "FTPS (explicit TLS)");
                    ui.add_enabled(s.secure, egui::Checkbox::new(&mut s.insecure_tls, "Accept self-signed / mismatching certificate"));
                    ui.horizontal(|ui| {
                        if ui.button("Connect").clicked() {
                            if s.host.trim().is_empty() {
                                self.message = Some("Enter a host.".into());
                            } else {
                                connect = Some(s.target());
                            }
                        }
                        if ui.button("Save").clicked() {
                            save_now = true;
                        }
                    });
                });
            });
            if let Some(m) = &self.message {
                ui.colored_label(ui.visuals().error_fg_color, m);
            }
            ui.weak("Passwords are saved encrypted for your Windows account only. Esc closes.");
            });
        });
        if save_now {
            self.save();
        }
        if save_url_session {
            match self.target_from_url() {
                Ok(t) => {
                    self.sessions.push(Session::from_target(&t));
                    self.sel = Some(self.sessions.len() - 1);
                    self.save();
                }
                Err(e) => self.message = Some(e),
            }
        }
        if connect.is_some() {
            // Edits made in the form are kept for next time.
            self.save();
            open = false;
        }
        self.open = open && !ctx.input(|i| i.key_pressed(Key::Escape));
        connect
    }
}
