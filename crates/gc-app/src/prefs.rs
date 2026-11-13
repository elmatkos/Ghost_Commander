//! Small preferences of the viewer and the editor that are remembered in `settings.toml`
//! (line wrapping, auto-reload of the viewer). Read from the file once, then kept in memory.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use crate::theme;

struct Prefs {
    wrap: AtomicBool,
    reload: AtomicBool,
    pdf_turn: AtomicBool,
}

fn prefs() -> &'static Prefs {
    static P: OnceLock<Prefs> = OnceLock::new();
    P.get_or_init(|| {
        let s = theme::load_settings();
        Prefs { wrap: AtomicBool::new(s.wrap_lines), reload: AtomicBool::new(s.auto_reload), pdf_turn: AtomicBool::new(s.pdf_turn_pages) }
    })
}

/// Long lines are wrapped in the viewer and the editor.
pub fn wrap() -> bool {
    prefs().wrap.load(Ordering::Relaxed)
}

/// The viewer reloads a file that changed on disk (and follows its end).
pub fn auto_reload() -> bool {
    prefs().reload.load(Ordering::Relaxed)
}

/// The PDF viewer's arrow keys continue on the next / previous page.
pub fn pdf_turn_pages() -> bool {
    prefs().pdf_turn.load(Ordering::Relaxed)
}

/// Updates the in-memory value only (the Options window saves the settings file itself).
pub fn set_pdf_turn_pages_cached(on: bool) {
    prefs().pdf_turn.store(on, Ordering::Relaxed);
}

pub fn set_wrap(on: bool) {
    prefs().wrap.store(on, Ordering::Relaxed);
    let mut s = theme::load_settings();
    s.wrap_lines = on;
    let _ = theme::save_settings(&s);
}

pub fn set_auto_reload(on: bool) {
    prefs().reload.store(on, Ordering::Relaxed);
    let mut s = theme::load_settings();
    s.auto_reload = on;
    let _ = theme::save_settings(&s);
}
