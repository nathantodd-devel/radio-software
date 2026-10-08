//! The colours of the app, and themes that change them: a few built in,
//! and any JSON files in the `themes` directory beside the channel database.
//!
//! A theme file is one object giving any of the colours below as
//! `"#rrggbb"`; the ones it leaves out keep their default.
//!
//! ```json
//! { "background": "#fbfbfa", "text": "#1d2129", "accent": "#1f8f5f" }
//! ```

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use scanner::db::Db;

/// Name of the theme the app starts out with.
pub const DEFAULT: &str = "Default";

/// The other themes that come with the app, as (name, theme file). The
/// Catppuccin ones use that project's palettes (catppuccin.com).
const BUILT_IN: &[(&str, &str)] = &[
    ("Light", include_str!("../themes/light.json")),
    ("Catppuccin Latte", include_str!("../themes/catppuccin-latte.json")),
    ("Catppuccin Frappé", include_str!("../themes/catppuccin-frappe.json")),
    (
        "Catppuccin Macchiato",
        include_str!("../themes/catppuccin-macchiato.json"),
    ),
    ("Catppuccin Mocha", include_str!("../themes/catppuccin-mocha.json")),
];

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    /// Behind everything.
    pub bg: u32,
    /// The header, sidebar and activity log.
    pub panel: u32,
    /// Buttons, meters and the row that is playing.
    pub raised: u32,
    pub border: u32,
    pub text: u32,
    /// Secondary text.
    pub muted: u32,
    /// Active channels and switched-on controls.
    pub accent: u32,
    /// A held channel.
    pub hold: u32,
    /// Priority channels and replay.
    pub priority: u32,
    /// Errors, and the Skip, Rec and Mute buttons when on.
    pub error: u32,
}

impl Theme {
    const DEFAULT: Self = Self {
        bg: 0x14171c,
        panel: 0x1c2027,
        raised: 0x2a303a,
        border: 0x313743,
        text: 0xe6e9ee,
        muted: 0x8b93a1,
        accent: 0x4cc38a,
        hold: 0xe0a84c,
        priority: 0x6aa6ff,
        error: 0xe5645c,
    };

    /// The colours by the names theme files use for them.
    fn fields(&mut self) -> [(&'static str, &mut u32); 10] {
        [
            ("background", &mut self.bg),
            ("panel", &mut self.panel),
            ("raised", &mut self.raised),
            ("border", &mut self.border),
            ("text", &mut self.text),
            ("muted", &mut self.muted),
            ("accent", &mut self.accent),
            ("hold", &mut self.hold),
            ("priority", &mut self.priority),
            ("error", &mut self.error),
        ]
    }

    /// Read a theme file's text; `name` is for messages.
    fn parse(json: &str, name: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("{name}: {e}"))?;
        let colors = value
            .as_object()
            .ok_or(format!("{name}: expected an object of colours"))?;
        let mut theme = Self::DEFAULT;
        let mut fields = theme.fields();
        for (key, value) in colors {
            let Some((_, field)) = fields.iter_mut().find(|(name, _)| name == key) else {
                let known: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
                return Err(format!(
                    "{name}: unknown colour {key:?} (there are: {})",
                    known.join(", ")
                ));
            };
            let hex = value
                .as_str()
                .map(|s| s.trim_start_matches('#'))
                .filter(|s| s.len() == 6);
            **field = hex
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .ok_or(format!("{name}: {key} must be a colour like \"#1c2027\""))?;
        }
        Ok(theme)
    }
}

static CURRENT: RwLock<Theme> = RwLock::new(Theme::DEFAULT);

/// The theme in use.
pub fn theme() -> Theme {
    *CURRENT.read().unwrap()
}

/// Where theme files are looked for.
pub fn dir() -> PathBuf {
    Db::default_path().with_file_name("themes")
}

/// The themes that can be chosen: the ones that come with the app, then
/// the files found.
pub fn available() -> Vec<String> {
    let mut names: Vec<String> = std::iter::once(DEFAULT)
        .chain(BUILT_IN.iter().map(|(name, _)| *name))
        .map(String::from)
        .collect();
    let mut files: Vec<String> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| Some(path.file_stem()?.to_str()?.to_string()))
        // A file named like a built-in theme replaces it rather than adding one.
        .filter(|name| !names.contains(name))
        .collect();
    files.sort();
    names.extend(files);
    names
}

/// Switch to the theme called `name`: the file of that name if there is
/// one, otherwise the theme of that name that comes with the app. If it
/// can't be used, says why and switches to the default.
pub fn select(name: &str) -> Result<(), String> {
    // Names come from file names in `dir()`, never paths.
    let path = dir()
        .join(Path::new(name).file_name().unwrap_or_default())
        .with_extension("json");
    let built_in = BUILT_IN.iter().find(|(built_in, _)| *built_in == name);
    let loaded = match (std::fs::read_to_string(&path), built_in) {
        (Ok(json), _) => Theme::parse(&json, &path.display().to_string()),
        (Err(_), Some((name, json))) => Theme::parse(json, name),
        (Err(_), None) if name == DEFAULT => Ok(Theme::DEFAULT),
        (Err(e), None) => Err(format!("{}: {e}", path.display())),
    };
    *CURRENT.write().unwrap() = *loaded.as_ref().unwrap_or(&Theme::DEFAULT);
    loaded.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_theme_files() {
        let theme = Theme::parse(r##"{ "background": "#fbfbfa", "accent": "1F8F5F" }"##, "t").unwrap();
        assert_eq!((theme.bg, theme.accent), (0xfbfbfa, 0x1f8f5f));
        assert_eq!(theme.text, Theme::DEFAULT.text);
        assert!(
            Theme::parse(r##"{ "backgrund": "#ffffff" }"##, "t")
                .unwrap_err()
                .contains("unknown colour")
        );
        assert!(Theme::parse(r##"{ "text": "white" }"##, "t").is_err());
        assert!(Theme::parse("[]", "t").is_err());
        // Every theme that comes with the app is valid and sets every colour.
        for (name, json) in BUILT_IN {
            Theme::parse(json, name).unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(json)
                    .unwrap()
                    .as_object()
                    .unwrap()
                    .len(),
                10
            );
        }
        assert!(available().starts_with(&["Default".to_string(), "Light".to_string()]));
    }
}
