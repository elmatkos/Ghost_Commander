//! Color themes: semantic colors, per-file-type icon + color, user extension mapping.
//! Built-in themes live in code; custom themes are TOML files in the config folder.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use eframe::egui::{self, Color32};
use gc_core::filetype::{extension_of, Category};
use gc_core::Entry;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Opaque color stored as `#rrggbb` in theme files.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hex(pub Color32);

impl Serialize for Hex {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("#{:02x}{:02x}{:02x}", self.0.r(), self.0.g(), self.0.b()))
    }
}

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        parse_hex(&s).map(Hex).ok_or_else(|| serde::de::Error::custom(format!("invalid color '{s}', expected #rrggbb")))
    }
}

fn parse_hex(s: &str) -> Option<Color32> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 || !h.is_ascii() {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct TypeStyle {
    pub color: Hex,
    /// Icon name from [`ICON_NAMES`] or a literal glyph character.
    pub icon: String,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    /// Base egui visuals: true = dark, false = light.
    pub dark: bool,
    pub background: Hex,
    pub text: Hex,
    /// Cursor row and selection.
    pub accent: Hex,
    pub dir: Hex,
    pub marked: Hex,
    pub error: Hex,
    /// Style per category name (see `Category::name`). Missing categories get defaults.
    #[serde(default)]
    pub types: BTreeMap<String, TypeStyle>,
    /// Extension (lowercase, no dot) -> category name; overrides the built-in mapping.
    #[serde(default)]
    pub extensions: BTreeMap<String, String>,
}

const fn rgb(r: u8, g: u8, b: u8) -> Hex {
    Hex(Color32::from_rgb(r, g, b))
}

/// Icon names usable in theme files. Anything else is treated as a literal glyph.
pub const ICON_NAMES: &[(&str, &str)] = &[
    ("folder", egui_phosphor::regular::FOLDER),
    ("archive", egui_phosphor::regular::FILE_ZIP),
    ("image", egui_phosphor::regular::IMAGE),
    ("audio", egui_phosphor::regular::MUSIC_NOTES),
    ("video", egui_phosphor::regular::FILM_STRIP),
    ("document", egui_phosphor::regular::FILE_DOC),
    ("code", egui_phosphor::regular::FILE_CODE),
    ("text", egui_phosphor::regular::FILE_TEXT),
    ("executable", egui_phosphor::regular::APP_WINDOW),
    ("config", egui_phosphor::regular::SLIDERS),
    ("file", egui_phosphor::regular::FILE),
    ("terminal", egui_phosphor::regular::TERMINAL_WINDOW),
    ("pdf", egui_phosphor::regular::FILE_PDF),
    ("sheet", egui_phosphor::regular::FILE_XLS),
    ("slides", egui_phosphor::regular::FILE_PPT),
    ("database", egui_phosphor::regular::DATABASE),
    ("package", egui_phosphor::regular::PACKAGE),
    ("disc", egui_phosphor::regular::DISC),
    ("book", egui_phosphor::regular::BOOK),
    ("cube", egui_phosphor::regular::CUBE),
    ("key", egui_phosphor::regular::KEY),
    ("lock", egui_phosphor::regular::LOCK),
    ("star", egui_phosphor::regular::STAR),
    ("heart", egui_phosphor::regular::HEART),
];

/// Resolves an icon name to its glyph; unknown text is used as is.
pub fn resolve_icon(icon: &str) -> &str {
    ICON_NAMES.iter().find(|(n, _)| n.eq_ignore_ascii_case(icon.trim())).map(|(_, g)| *g).unwrap_or(icon)
}

fn default_icon(cat: Category) -> &'static str {
    match cat {
        Category::Folder => "folder",
        Category::Archive => "archive",
        Category::Image => "image",
        Category::Audio => "audio",
        Category::Video => "video",
        Category::Document => "document",
        Category::Code => "code",
        Category::Text => "text",
        Category::Executable => "executable",
        Category::Config => "config",
        Category::Other => "file",
    }
}

/// Category colors tuned for a dark or a light ground.
fn default_color(cat: Category, dark: bool) -> Hex {
    let (d, l) = match cat {
        Category::Folder => (rgb(140, 200, 255), rgb(20, 100, 180)),
        Category::Archive => (rgb(229, 160, 75), rgb(176, 98, 0)),
        Category::Image => (rgb(95, 211, 141), rgb(21, 128, 61)),
        Category::Audio => (rgb(197, 140, 240), rgb(126, 34, 206)),
        Category::Video => (rgb(240, 124, 124), rgb(185, 28, 28)),
        Category::Document => (rgb(111, 168, 255), rgb(29, 78, 216)),
        Category::Code => (rgb(79, 209, 197), rgb(15, 118, 110)),
        Category::Text => (rgb(201, 209, 217), rgb(75, 85, 99)),
        Category::Executable => (rgb(255, 140, 90), rgb(194, 65, 12)),
        Category::Config => (rgb(160, 168, 180), rgb(107, 114, 128)),
        Category::Other => (rgb(154, 164, 178), rgb(107, 114, 128)),
    };
    if dark {
        d
    } else {
        l
    }
}

fn default_types(dark: bool) -> BTreeMap<String, TypeStyle> {
    Category::ALL
        .into_iter()
        .map(|c| (c.name().to_owned(), TypeStyle { color: default_color(c, dark), icon: default_icon(c).to_owned() }))
        .collect()
}

impl Theme {
    #[allow(clippy::too_many_arguments)]
    fn builtin(name: &str, dark: bool, background: Hex, text: Hex, accent: Hex, dir: Hex, marked: Hex, error: Hex) -> Theme {
        Theme {
            name: name.to_owned(),
            dark,
            background,
            text,
            accent,
            dir,
            marked,
            error,
            types: default_types(dark),
            extensions: BTreeMap::new(),
        }
    }

    pub fn builtins() -> Vec<Theme> {
        let mut norton = Theme::builtin(
            "Norton Classic",
            true,
            rgb(0, 0, 170),
            rgb(85, 255, 255),
            rgb(0, 170, 170),
            rgb(255, 255, 255),
            rgb(255, 255, 85),
            rgb(255, 85, 85),
        );
        // The classic look is two-tone: every type uses the text color except folders.
        for style in norton.types.values_mut() {
            style.color = rgb(85, 255, 255);
        }
        vec![
            Theme::builtin("Dark", true, rgb(27, 27, 31), rgb(204, 204, 204), rgb(47, 93, 158), rgb(140, 200, 255), rgb(255, 210, 90), rgb(255, 123, 123)),
            Theme::builtin("Light", false, rgb(243, 243, 243), rgb(30, 30, 30), rgb(156, 196, 255), rgb(11, 92, 173), rgb(180, 95, 0), rgb(179, 38, 30)),
            Theme::builtin("Ghost", true, rgb(10, 18, 38), rgb(230, 240, 255), rgb(31, 86, 196), rgb(65, 200, 255), rgb(255, 224, 102), rgb(255, 107, 107)),
            norton,
        ]
    }

    pub fn is_builtin_name(name: &str) -> bool {
        Theme::builtins().iter().any(|t| t.name.eq_ignore_ascii_case(name.trim()))
    }

    /// Adds default styles for categories a theme file does not mention.
    pub fn fill_missing(&mut self) {
        for (name, style) in default_types(self.dark) {
            self.types.entry(name).or_insert(style);
        }
    }

    /// Category of an entry, honoring the theme's extension overrides.
    pub fn category_of(&self, e: &Entry) -> Category {
        if e.is_dir {
            return Category::Folder;
        }
        let Some(ext) = extension_of(&e.path) else { return Category::Other };
        self.extensions
            .get(&ext)
            .and_then(|c| Category::from_name(c))
            .unwrap_or_else(|| Category::from_extension(&ext))
    }

    /// Icon glyph and color for a category.
    pub fn style(&self, cat: Category) -> (&str, Color32) {
        match self.types.get(cat.name()) {
            Some(s) => (resolve_icon(&s.icon), s.color.0),
            None => (resolve_icon(default_icon(cat)), default_color(cat, self.dark).0),
        }
    }

    pub fn apply(&self, ctx: &egui::Context) {
        let mut v = if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        v.panel_fill = self.background.0;
        v.window_fill = self.background.0;
        v.extreme_bg_color = self.background.0;
        v.override_text_color = Some(self.text.0);
        v.selection.bg_fill = self.accent.0;
        v.hyperlink_color = self.accent.0;
        // Pin egui's mode to the theme's base: otherwise egui follows the system theme (WinPE reports
        // a different one) and the first frame switches away from the visuals set here.
        ctx.set_theme(if self.dark { egui::Theme::Dark } else { egui::Theme::Light });
        ctx.set_visuals(v);
    }
}

/// `ext=category` lines <-> map, for the editor's extension box.
pub fn extensions_to_text(map: &BTreeMap<String, String>) -> String {
    map.iter().map(|(e, c)| format!("{e}={c}\n")).collect()
}

/// Lines that are not `ext=known_category` are ignored.
pub fn extensions_from_text(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| {
            let (ext, cat) = l.split_once('=')?;
            let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
            let cat = Category::from_name(cat)?;
            (!ext.is_empty()).then(|| (ext, cat.name().to_owned()))
        })
        .collect()
}

// ---- persistence -----------------------------------------------------------

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Option<String>,
    /// Drive buttons above each panel.
    pub drive_buttons: bool,
    /// Free-space bar under each drive button.
    pub drive_free_bar: bool,
    /// Folders the left / right panel showed when the app was last used.
    pub left_dir: Option<String>,
    pub right_dir: Option<String>,
    /// Open tabs of each side (folders) and which one was shown.
    pub left_tabs: Vec<String>,
    pub right_tabs: Vec<String>,
    pub left_tab: usize,
    pub right_tab: usize,
    /// When the folder or drive a panel shows disappears (USB stick pulled): false = keep showing it
    /// and reload as soon as it is back, true = switch to the nearest existing folder / drive.
    pub switch_on_missing_drive: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { theme: None, drive_buttons: true, drive_free_bar: true, left_dir: None, right_dir: None, left_tabs: Vec::new(), right_tabs: Vec::new(), left_tab: 0, right_tab: 0, switch_on_missing_drive: false }
    }
}

pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Ghost Commander"))
}

fn themes_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join("themes"))
}

pub fn load_settings() -> Settings {
    config_dir()
        .and_then(|d| fs::read_to_string(d.join("settings.toml")).ok())
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_settings(settings: &Settings) -> io::Result<()> {
    let dir = config_dir().ok_or_else(|| io::Error::other("APPDATA is not set"))?;
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("settings.toml"), toml::to_string(settings).map_err(io::Error::other)?)
}

/// Loads `*.toml` themes from the themes folder; broken files are reported, not fatal.
pub fn load_custom_themes() -> (Vec<Theme>, Vec<String>) {
    let mut themes = Vec::new();
    let mut problems = Vec::new();
    let Some(dir) = themes_dir() else { return (themes, problems) };
    let Ok(rd) = fs::read_dir(&dir) else { return (themes, problems) };
    for entry in rd.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().is_none_or(|e| !e.eq_ignore_ascii_case("toml")) {
            continue;
        }
        match fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| toml::from_str::<Theme>(&s).map_err(|e| e.to_string())) {
            Ok(mut t) if !Theme::is_builtin_name(&t.name) => {
                t.fill_missing();
                themes.push(t);
            }
            Ok(t) => problems.push(format!("{}: name '{}' is reserved for a built-in theme", path.display(), t.name)),
            Err(e) => problems.push(format!("{}: {e}", path.display())),
        }
    }
    themes.sort_by_key(|t| t.name.to_lowercase());
    (themes, problems)
}

/// Writes a custom theme to the themes folder.
pub fn save_custom_theme(theme: &Theme) -> io::Result<PathBuf> {
    let name = theme.name.trim();
    if name.is_empty() || Theme::is_builtin_name(name) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "choose a name that is not empty or a built-in theme"));
    }
    let file: String = name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' }).collect();
    let dir = themes_dir().ok_or_else(|| io::Error::other("APPDATA is not set"))?;
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{file}.toml"));
    fs::write(&path, toml::to_string_pretty(theme).map_err(io::Error::other)?)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn entry(name: &str, is_dir: bool) -> Entry {
        Entry { name: name.into(), path: PathBuf::from(name), is_dir, size: 0, modified: None::<SystemTime> }
    }

    #[test]
    fn hex_round_trips_and_rejects_garbage() {
        assert_eq!(parse_hex("#0a1226"), Some(Color32::from_rgb(10, 18, 38)));
        assert_eq!(parse_hex("FFFFFF"), Some(Color32::WHITE));
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("#gggggg"), None);
    }

    #[test]
    fn theme_survives_toml_round_trip() {
        for t in Theme::builtins() {
            let text = toml::to_string_pretty(&t).unwrap();
            let back: Theme = toml::from_str(&text).unwrap();
            assert_eq!(t, back, "{}", t.name);
        }
    }

    #[test]
    fn minimal_theme_file_gets_default_type_styles() {
        let src = "name = \"Mine\"\ndark = true\nbackground = \"#000000\"\ntext = \"#ffffff\"\naccent = \"#0000ff\"\ndir = \"#00ffff\"\nmarked = \"#ffff00\"\nerror = \"#ff0000\"\n";
        let mut t: Theme = toml::from_str(src).unwrap();
        assert!(t.types.is_empty());
        t.fill_missing();
        assert_eq!(t.types.len(), Category::ALL.len());
    }

    #[test]
    fn extension_override_beats_builtin_mapping() {
        let mut t = Theme::builtins().remove(0);
        assert_eq!(t.category_of(&entry("a.log", false)), Category::Text);
        t.extensions = extensions_from_text("LOG = code\n.bak=archive\nbad line\nfoo=nonsense\n");
        assert_eq!(t.extensions.len(), 2);
        assert_eq!(t.category_of(&entry("a.log", false)), Category::Code);
        assert_eq!(t.category_of(&entry("x.bak", false)), Category::Archive);
        assert_eq!(t.category_of(&entry("dir", true)), Category::Folder);
        assert_eq!(t.category_of(&entry("Makefile", false)), Category::Other);
    }

    #[test]
    fn icon_names_resolve_and_literals_pass_through() {
        assert_eq!(resolve_icon("folder"), egui_phosphor::regular::FOLDER);
        assert_eq!(resolve_icon(" Star "), egui_phosphor::regular::STAR);
        assert_eq!(resolve_icon("X"), "X");
    }

    #[test]
    fn builtin_names_are_reserved() {
        assert!(Theme::is_builtin_name("dark"));
        let mut t = Theme::builtins().remove(0);
        t.name = "Ghost".into();
        assert!(save_custom_theme(&t).is_err());
        t.name = "  ".into();
        assert!(save_custom_theme(&t).is_err());
    }
}
