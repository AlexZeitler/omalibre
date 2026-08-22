//! Colours, taken from the active Omarchy theme.
//!
//! Omarchy renders `~/.config/omarchy/themed/omalibre.toml.tpl` on every theme
//! change and writes the result into the current theme directory. The reader
//! reads that file and falls back to built-in colours when it is absent, so it
//! works on a machine without Omarchy just as well.
//!
//! The file is re-read when its timestamp changes. A theme change replaces the
//! whole directory by an atomic swap, so the path points at a new file; checking
//! the timestamp of the path catches that, where a watch on the file itself would
//! lose track of it.

use serde::Deserialize;
use std::path::PathBuf;
use std::time::SystemTime;

/// A colour as 24-bit RGB.
pub type Rgb = (u8, u8, u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub background: Rgb,
    pub foreground: Rgb,
    /// Headings and progress.
    pub accent: Rgb,
    /// Rules, image labels, the status line.
    pub muted: Rgb,
    pub code_background: Rgb,
    pub code_foreground: Rgb,
    pub quote: Rgb,
    /// The five annotation colours, in the order of `annotation::Color::ALL`.
    pub marks: [Rgb; 5],
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: (0x1a, 0x1b, 0x26),
            foreground: (0xc8, 0xd3, 0xf5),
            accent: (0xff, 0xc7, 0x77),
            muted: (0x7a, 0x88, 0xa8),
            code_background: (0x24, 0x28, 0x36),
            code_foreground: (0x86, 0xe1, 0xfc),
            quote: (0x9a, 0xa5, 0xce),
            marks: BUILT_IN_MARKS,
        }
    }
}

/// Annotation colours that are known to tell each other apart. Used when a theme
/// offers no five distinguishable ones.
const BUILT_IN_MARKS: [Rgb; 5] = [
    (0xd9, 0xb8, 0x4c), // yellow
    (0x6f, 0xb8, 0x6f), // green
    (0x6a, 0x9f, 0xd8), // blue
    (0xe0, 0x7a, 0x6a), // red
    (0xb4, 0x8a, 0xd8), // purple
];

/// The file as written on disk. Every field is optional, so a partial template
/// still works.
#[derive(Debug, Deserialize, Default)]
struct File {
    #[serde(default)]
    colors: Colors,
}

#[derive(Debug, Deserialize, Default)]
struct Colors {
    background: Option<String>,
    foreground: Option<String>,
    accent: Option<String>,
    muted: Option<String>,
    code_background: Option<String>,
    code_foreground: Option<String>,
    quote: Option<String>,
    mark_yellow: Option<String>,
    mark_green: Option<String>,
    mark_blue: Option<String>,
    mark_red: Option<String>,
    mark_purple: Option<String>,
}

/// Watches the theme file and hands out the current colours.
pub struct Watcher {
    path: Option<PathBuf>,
    seen: Option<SystemTime>,
    theme: Theme,
}

impl Watcher {
    pub fn new() -> Self {
        let mut watcher = Self {
            path: theme_file(),
            seen: None,
            theme: Theme::default(),
        };
        watcher.reload();
        watcher
    }

    pub fn theme(&self) -> Theme {
        self.theme
    }

    /// Re-reads the file if it changed. Returns true when the colours moved.
    pub fn refresh(&mut self) -> bool {
        let Some(path) = &self.path else {
            return false;
        };
        let stamp = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        if stamp == self.seen {
            return false;
        }
        let before = self.theme;
        self.reload();
        before != self.theme
    }

    fn reload(&mut self) {
        let Some(path) = &self.path else { return };
        self.seen = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let Ok(text) = std::fs::read_to_string(path) else {
            self.theme = Theme::default();
            return;
        };
        self.theme = parse(&text);
    }
}

/// Where the rendered theme file lives. Quattro keeps the current theme in the
/// state directory; earlier releases used the config directory.
fn theme_file() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let candidates = [
        home.join(".local/state/omarchy/current/theme/omalibre.toml"),
        home.join(".config/omarchy/current/theme/omalibre.toml"),
    ];
    candidates.into_iter().find(|path| path.exists())
}

/// The template Omarchy renders on every theme change. It is built into the
/// binary, so a downloaded reader sets itself up without a checkout.
const TEMPLATE: &str = include_str!("../themed/omalibre.toml.tpl");

/// Puts the template in place, so the reader follows the theme from the first
/// start onwards. Reports whether it wrote something.
///
/// Nothing happens when the file is already there. It may be a link into a
/// checkout, or hold colours someone changed on purpose, and neither should be
/// overwritten by a program start.
pub fn install_template() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    if !write_template(&home.join(".config/omarchy/themed/omalibre.toml.tpl")) {
        return false;
    }
    // Without this the template stays unrendered until the next theme change,
    // which would leave the first session with the built-in colours.
    if let Some(theme) = current_theme_name(&home) {
        apply_theme(&theme);
    }
    true
}

/// Writes the template unless something is there already.
fn write_template(target: &std::path::Path) -> bool {
    // symlink_metadata rather than exists: a link into a checkout counts as
    // present even where its target moved away.
    if target.symlink_metadata().is_ok() {
        return false;
    }
    let Some(themed) = target.parent() else {
        return false;
    };
    // Only where Omarchy lives. Elsewhere nothing would render the template and
    // the directory would be litter.
    if !themed.parent().is_some_and(|omarchy| omarchy.is_dir()) {
        return false;
    }
    std::fs::create_dir_all(themed).is_ok() && std::fs::write(target, TEMPLATE).is_ok()
}

/// The theme in use, from wherever this Omarchy release records it.
fn current_theme_name(home: &std::path::Path) -> Option<String> {
    let candidates = [
        home.join(".local/state/omarchy/current/theme.name"),
        home.join(".config/omarchy/current/theme.name"),
    ];
    candidates
        .into_iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// Re-applies a theme, which renders every template including ours.
///
/// The wallpaper is left alone: re-applying it would flash the desktop for a
/// step that is about text colours. Output is discarded because the reader is
/// about to take over the screen.
fn apply_theme(name: &str) {
    let _ = std::process::Command::new("omarchy-theme-set")
        .arg(name)
        .env("OMARCHY_THEME_SKIP_BACKGROUND", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn parse(text: &str) -> Theme {
    let file: File = match toml::from_str(text) {
        Ok(file) => file,
        // A malformed file must not leave the reader colourless.
        Err(_) => return Theme::default(),
    };
    let fallback = Theme::default();
    let colors = file.colors;
    let pick =
        |value: &Option<String>, default: Rgb| value.as_deref().and_then(hex).unwrap_or(default);

    let marks = [
        pick(&colors.mark_yellow, BUILT_IN_MARKS[0]),
        pick(&colors.mark_green, BUILT_IN_MARKS[1]),
        pick(&colors.mark_blue, BUILT_IN_MARKS[2]),
        pick(&colors.mark_red, BUILT_IN_MARKS[3]),
        pick(&colors.mark_purple, BUILT_IN_MARKS[4]),
    ];

    Theme {
        background: pick(&colors.background, fallback.background),
        foreground: pick(&colors.foreground, fallback.foreground),
        accent: pick(&colors.accent, fallback.accent),
        muted: pick(&colors.muted, fallback.muted),
        code_background: pick(&colors.code_background, fallback.code_background),
        code_foreground: pick(&colors.code_foreground, fallback.code_foreground),
        quote: pick(&colors.quote, fallback.quote),
        // Annotation colours carry meaning, so they have to be told apart. Not
        // every theme offers five that are: some map red and purple onto one
        // accent. Where that happens the built-in set is used as a whole, so the
        // five stay a coherent scale instead of a mixture.
        marks: if all_distinguishable(&marks) {
            marks
        } else {
            BUILT_IN_MARKS
        },
    }
}

/// Squared distance in RGB below which two colours read as the same mark.
const MIN_MARK_DISTANCE: u32 = 3000;

fn all_distinguishable(marks: &[Rgb; 5]) -> bool {
    for (i, a) in marks.iter().enumerate() {
        for b in marks.iter().skip(i + 1) {
            if distance(*a, *b) < MIN_MARK_DISTANCE {
                return false;
            }
        }
    }
    true
}

fn distance((ar, ag, ab): Rgb, (br, bg, bb): Rgb) -> u32 {
    let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2) as u32;
    d(ar, br) + d(ag, bg) + d(ab, bb)
}

/// Parses `#rrggbb`, with or without the hash.
fn hex(text: &str) -> Option<Rgb> {
    let text = text.trim().trim_start_matches('#');
    if text.len() != 6 || !text.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(&text[at..at + 2], 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway home directory, named after the test that uses it.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omalibre-theme-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_template_lands_where_omarchy_renders_it() {
        let home = scratch("install");
        std::fs::create_dir_all(home.join(".config/omarchy")).unwrap();
        let target = home.join(".config/omarchy/themed/omalibre.toml.tpl");

        assert!(write_template(&target));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), TEMPLATE);
    }

    #[test]
    fn an_existing_template_is_left_alone() {
        let home = scratch("keep");
        std::fs::create_dir_all(home.join(".config/omarchy/themed")).unwrap();
        let target = home.join(".config/omarchy/themed/omalibre.toml.tpl");
        std::fs::write(&target, "mine").unwrap();

        assert!(!write_template(&target));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "mine");
    }

    #[test]
    fn nothing_is_written_without_omarchy() {
        let home = scratch("no-omarchy");
        std::fs::create_dir_all(home.join(".config")).unwrap();
        let target = home.join(".config/omarchy/themed/omalibre.toml.tpl");

        assert!(!write_template(&target));
        assert!(!home.join(".config/omarchy").exists());
    }

    #[test]
    fn the_theme_name_comes_from_the_state_directory() {
        let home = scratch("name");
        std::fs::create_dir_all(home.join(".local/state/omarchy/current")).unwrap();
        std::fs::write(
            home.join(".local/state/omarchy/current/theme.name"),
            "giants\n",
        )
        .unwrap();

        assert_eq!(current_theme_name(&home).as_deref(), Some("giants"));
    }

    #[test]
    fn an_older_layout_still_yields_the_theme_name() {
        let home = scratch("name-old");
        std::fs::create_dir_all(home.join(".config/omarchy/current")).unwrap();
        std::fs::write(
            home.join(".config/omarchy/current/theme.name"),
            "matte-black",
        )
        .unwrap();

        assert_eq!(current_theme_name(&home).as_deref(), Some("matte-black"));
    }

    #[test]
    fn the_template_renders_every_colour_the_reader_reads() {
        // The template is the only source of the theme file, so a field the
        // reader parses but the template never writes would silently fall back.
        for field in [
            "background",
            "foreground",
            "accent",
            "muted",
            "code_background",
            "code_foreground",
            "quote",
            "mark_yellow",
            "mark_green",
            "mark_blue",
            "mark_red",
            "mark_purple",
        ] {
            assert!(
                TEMPLATE.contains(&format!("{field} = ")),
                "{field} is missing"
            );
        }
    }

    #[test]
    fn reads_a_rendered_file() {
        let theme = parse(
            r##"
            [colors]
            background = "#0A1428"
            foreground = "#F0F8FF"
            accent = "#FF40A3"
            muted = "#88919e"
            code_background = "#212b3e"
            code_foreground = "#f4cae8"
            quote = "#abb4be"
            mark_yellow = "#d9b84c"
            mark_green = "#6fb86f"
            mark_blue = "#6a9fd8"
            mark_red = "#e07a6a"
            mark_purple = "#b48ad8"
            "##,
        );
        assert_eq!(theme.background, (0x0a, 0x14, 0x28));
        assert_eq!(theme.accent, (0xff, 0x40, 0xa3));
        assert_eq!(theme.marks[0], (0xd9, 0xb8, 0x4c));
    }

    #[test]
    fn missing_entries_keep_their_defaults() {
        let theme = parse("[colors]\naccent = \"#ff0000\"\n");
        assert_eq!(theme.accent, (0xff, 0, 0));
        assert_eq!(theme.background, Theme::default().background);
    }

    #[test]
    fn a_malformed_file_leaves_the_defaults() {
        assert_eq!(parse("this is not toml {{{"), Theme::default());
        assert_eq!(parse(""), Theme::default());
    }

    #[test]
    fn marks_that_collide_fall_back_as_a_set() {
        // A theme where red and purple are the same accent, as some are.
        let theme = parse(
            r##"
            [colors]
            mark_yellow = "#5076B2"
            mark_green = "#00BFFF"
            mark_blue = "#F0F8FF"
            mark_red = "#FF40A3"
            mark_purple = "#FF40A3"
            "##,
        );
        assert_eq!(
            theme.marks, BUILT_IN_MARKS,
            "colliding marks must be dropped"
        );
    }

    #[test]
    fn the_built_in_marks_are_distinguishable() {
        assert!(all_distinguishable(&BUILT_IN_MARKS));
    }

    #[test]
    fn every_mark_carries_readable_text() {
        // WCAG AA asks for 4.5:1 on body text. A highlight sets both colours, so
        // this has to hold for the text drawn on it.
        for mark in BUILT_IN_MARKS {
            let fg = crate::annotation::text_color_on(mark);
            let ratio = crate::annotation::contrast_ratio(mark, fg);
            assert!(ratio >= 4.5, "{mark:?} reaches only {ratio:.2}:1");
        }
    }

    #[test]
    fn parses_hex_with_and_without_hash() {
        assert_eq!(hex("#a1b2c3"), Some((0xa1, 0xb2, 0xc3)));
        assert_eq!(hex("A1B2C3"), Some((0xa1, 0xb2, 0xc3)));
        assert_eq!(hex(" #a1b2c3 "), Some((0xa1, 0xb2, 0xc3)));
        assert_eq!(hex("#abc"), None);
        assert_eq!(hex("#gggggg"), None);
        assert_eq!(hex(""), None);
    }
}
