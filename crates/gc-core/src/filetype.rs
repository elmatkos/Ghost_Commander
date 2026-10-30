//! File categories by extension. Used for icons and colors; themes can remap extensions.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Folder,
    Archive,
    Image,
    Audio,
    Video,
    Document,
    Code,
    Text,
    Executable,
    Config,
    Other,
}

impl Category {
    pub const ALL: [Category; 11] = [
        Category::Folder,
        Category::Archive,
        Category::Image,
        Category::Audio,
        Category::Video,
        Category::Document,
        Category::Code,
        Category::Text,
        Category::Executable,
        Category::Config,
        Category::Other,
    ];

    /// Stable lowercase name, used as the key in theme files.
    pub fn name(self) -> &'static str {
        match self {
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
            Category::Other => "other",
        }
    }

    pub fn from_name(name: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.name().eq_ignore_ascii_case(name.trim()))
    }

    /// Built-in category of a file extension (lowercase, without the dot).
    pub fn from_extension(ext: &str) -> Category {
        match ext {
            "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "cab" | "iso" | "lz4" | "wim" => Category::Archive,
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff" | "avif" | "svg" | "heic" => Category::Image,
            "mp3" | "flac" | "wav" | "ogg" | "m4a" | "aac" | "opus" | "wma" => Category::Audio,
            "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "flv" | "m4v" => Category::Video,
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "rtf" => Category::Document,
            "rs" | "c" | "h" | "cpp" | "hpp" | "cs" | "py" | "js" | "ts" | "java" | "go" | "html" | "css" | "ps1" | "sh" | "bat" | "cmd" | "asm" => Category::Code,
            "txt" | "md" | "log" | "csv" | "nfo" => Category::Text,
            "exe" | "msi" | "dll" | "sys" | "com" | "scr" | "efi" => Category::Executable,
            "toml" | "json" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "xml" | "reg" | "lock" => Category::Config,
            _ => Category::Other,
        }
    }
}

/// Lowercase extension of a path, if any.
pub fn extension_of(path: &Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_map_to_categories() {
        assert_eq!(Category::from_extension("7z"), Category::Archive);
        assert_eq!(Category::from_extension("rs"), Category::Code);
        assert_eq!(Category::from_extension("xyz"), Category::Other);
    }

    #[test]
    fn extension_is_lowercased() {
        assert_eq!(extension_of(Path::new("A/B/Photo.JPG")).as_deref(), Some("jpg"));
        assert_eq!(extension_of(Path::new("Makefile")), None);
    }

    #[test]
    fn names_round_trip() {
        for c in Category::ALL {
            assert_eq!(Category::from_name(c.name()), Some(c));
        }
        assert_eq!(Category::from_name(" Image "), Some(Category::Image));
        assert_eq!(Category::from_name("nope"), None);
    }
}
