//! Monospace fonts installed on this computer, for the terminal font setting.
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, PartialEq)]
pub struct Font {
    pub family: String,
    pub path: PathBuf,
}

/// Fonts larger than this are skipped while looking: monospace fonts are small, and reading
/// every large font, such as those for Chinese or Japanese, would take long.
const LARGEST: u64 = 8 * 1024 * 1024;

/// Reads a font file for use, refusing what is not a font.
pub fn read(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("Cannot read the terminal font {}: {e}", path.display()))?;
    ttf_parser::Face::parse(&bytes, 0)
        .map_err(|_| format!("{} is not a font Relay can use.", path.display()))?;
    Ok(bytes)
}

/// The family name of the regular, upright, monospace font in `bytes`, if it is one.
fn monospace_family(bytes: &[u8]) -> Option<String> {
    let face = ttf_parser::Face::parse(bytes, 0).ok()?;
    if !face.is_monospaced() || face.is_bold() || face.is_italic() || face.is_oblique() {
        return None;
    }
    // Prefer the family as the font groups it, without a weight such as "Light".
    let name = |id| {
        (face.names().into_iter())
            .filter(|n| n.name_id == id && n.is_unicode())
            .find_map(|n| n.to_string())
    };
    name(ttf_parser::name_id::TYPOGRAPHIC_FAMILY)
        .or_else(|| name(ttf_parser::name_id::FAMILY))
        .filter(|family| !family.trim().is_empty())
}

/// The folders fonts are installed in on this platform.
fn folders() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    if cfg!(windows) {
        let root = std::env::var_os("WINDIR").unwrap_or_else(|| r"C:\Windows".into());
        let mut folders = vec![PathBuf::from(root).join("Fonts")];
        if let Some(local) = dirs::data_local_dir() {
            folders.push(local.join(r"Microsoft\Windows\Fonts"));
        }
        folders
    } else if cfg!(target_os = "macos") {
        vec![
            "/System/Library/Fonts".into(),
            "/Library/Fonts".into(),
            home.join("Library/Fonts"),
        ]
    } else {
        vec![
            "/usr/share/fonts".into(),
            "/usr/local/share/fonts".into(),
            home.join(".local/share/fonts"),
            home.join(".fonts"),
        ]
    }
}

/// Finds the installed monospace fonts in `folders`, one file per family, sorted by name.
fn find(folders: &[PathBuf]) -> Vec<Font> {
    let mut fonts: Vec<Font> = vec![];
    let mut pending: Vec<(PathBuf, u8)> = folders.iter().map(|f| (f.clone(), 0)).collect();
    while let Some((folder, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if depth < 4 {
                    pending.push((path, depth + 1));
                }
                continue;
            }
            let extension = (path.extension())
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !matches!(extension.as_str(), "ttf" | "otf" | "ttc") || meta.len() > LARGEST {
                continue;
            }
            let Some(family) = std::fs::read(&path)
                .ok()
                .and_then(|bytes| monospace_family(&bytes))
            else {
                continue;
            };
            if !fonts.iter().any(|f| f.family == family) {
                fonts.push(Font { family, path });
            }
        }
    }
    fonts.sort_by_key(|f| f.family.to_lowercase());
    fonts
}

/// Looks for installed fonts in the background, as reading them takes a moment.
#[derive(Clone, Default)]
pub struct Scan(Arc<Mutex<Option<Vec<Font>>>>);

impl Scan {
    pub fn start(ctx: eframe::egui::Context) -> Self {
        let scan = Self::default();
        let found = scan.0.clone();
        std::thread::spawn(move || {
            *found.lock().unwrap() = Some(find(&folders()));
            ctx.request_repaint();
        });
        scan
    }

    /// The fonts found, once the search is done.
    pub fn fonts(&self) -> Option<Vec<Font>> {
        self.0.lock().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_monospace_fonts_and_refuses_other_files() {
        // Whatever this computer has: every font found is a monospace regular face.
        let fonts = find(&folders());
        for font in &fonts {
            let bytes = read(&font.path).unwrap();
            assert!(monospace_family(&bytes).is_some(), "{font:?}");
        }
        let names: Vec<_> = fonts.iter().map(|f| f.family.to_lowercase()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(names, sorted, "sorted, one per family");
        let not_font = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert!(read(&not_font).unwrap_err().contains("not a font"));
        assert!(read(Path::new("/no/such/font.ttf")).is_err());
    }
}
