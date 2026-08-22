//! Where omalibre keeps its files, and the settings that point there.
//!
//! Three locations with three jobs, following the XDG base directories:
//!
//! - `~/.config/omalibre/config.toml` holds settings and belongs in version
//!   control.
//! - `~/.local/share/omalibre/` holds the read model, which is derived and may
//!   be deleted at any time.
//! - The journal directory holds the source of truth. It defaults to the data
//!   directory and should be moved to a synchronised folder when the reading
//!   position is meant to hold across machines.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const APP: &str = "omalibre";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// Directory holding the journal files. One file per machine.
    pub journal_dir: Option<PathBuf>,
    /// Maximum reading width in columns. `None` uses the full window.
    pub max_width: Option<u16>,
    /// How to draw pictures: `kitty`, `sixel` or `half-blocks`. `None` asks the
    /// terminal.
    pub images: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_file()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("{} is malformed", path.display()))
    }

    /// Writes a commented starter file, unless one exists already.
    pub fn write_default_if_missing() -> Result<()> {
        let path = config_file()?;
        if path.exists() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let default_journal = data_dir()?.join("journal");
        let contents = format!(
            "# omalibre settings\n\
             \n\
             # Where the journal lives. It is the source of truth for reading\n\
             # positions and annotations. Point this at a synchronised folder to\n\
             # carry your reading position between machines. Each machine writes\n\
             # only its own file, so no conflict can arise.\n\
             # journal_dir = \"{}\"\n\
             \n\
             # Reading width in columns. Comment out to use the full window.\n\
             # max_width = 66\n\
             \n\
             # How pictures are drawn. Left out, the terminal is asked and the\n\
             # best of kitty, sixel and half-blocks is used. Inside tmux only\n\
             # half-blocks work, because tmux manages the screen itself.\n\
             # images = \"sixel\"\n",
            default_journal.display()
        );
        std::fs::write(&path, contents)
            .with_context(|| format!("cannot write {}", path.display()))?;
        Ok(())
    }

    pub fn journal_dir(&self) -> Result<PathBuf> {
        match &self.journal_dir {
            Some(dir) => Ok(expand_tilde(dir)),
            None => Ok(data_dir()?.join("journal")),
        }
    }
}

pub fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

fn config_dir() -> Result<PathBuf> {
    dirs::config_dir()
        .map(|d| d.join(APP))
        .context("cannot determine the configuration directory")
}

/// Derived data: the read model and anything else that can be rebuilt.
pub fn data_dir() -> Result<PathBuf> {
    dirs::data_dir()
        .map(|d| d.join(APP))
        .context("cannot determine the data directory")
}

/// A private directory for short-lived files handed to another program.
///
/// The shared temporary directory will not do. A file there is readable by
/// every account on the machine, and the note handed to an editor carries both
/// what the reader wrote and the passage it belongs to. `XDG_RUNTIME_DIR` is
/// per-user and already mode 0700. Where it is absent, the data directory
/// serves: it sits under the home directory rather than in a world-writable
/// one.
pub fn scratch_dir() -> Result<PathBuf> {
    let dir = match dirs::runtime_dir() {
        Some(base) => base.join(APP),
        None => data_dir()?.join("scratch"),
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    // Set rather than assumed: create_dir_all applies the umask, and a
    // directory left over from an earlier run keeps whatever mode it had.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("cannot restrict {}", dir.display()))?;
    Ok(dir)
}

/// Replaces a leading `~` with the home directory.
fn expand_tilde(path: &PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix("~/") {
        Some(rest) => match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => path.clone(),
        },
        None => path.clone(),
    }
}

/// Name of this machine, used to name its journal file. Falls back to a fixed
/// name so a missing hostname never loses events.
pub fn hostname() -> String {
    let raw = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .unwrap_or_default();
    let name: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        "unknown-host".to_string()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_is_usable_in_a_filename() {
        let name = hostname();
        assert!(!name.is_empty());
        assert!(
            name.chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[test]
    fn reads_settings_from_toml() {
        let config: Config = toml::from_str("max_width = 72\njournal_dir = \"~/box\"\n").unwrap();
        assert_eq!(config.max_width, Some(72));
        assert_eq!(config.journal_dir, Some(PathBuf::from("~/box")));
    }

    #[test]
    fn missing_settings_fall_back_to_defaults() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.max_width, None);
        assert_eq!(config.journal_dir, None);
    }

    #[test]
    fn the_scratch_directory_admits_nobody_else() {
        let dir = scratch_dir().unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{} is {mode:o}", dir.display());
    }
}
