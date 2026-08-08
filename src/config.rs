//! Configuration: providers, start position and cache policy.
//!
//! Stored as TOML at `~/.config/waltzgps/config.toml` (or an explicit path
//! passed on the command line). A default file is written on first run.

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// A tile provider and its URL template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub name: String,
    /// URL template using `{z}`/`{x}`/`{y}` placeholders.
    pub url: String,
    /// When true, use TMS Y-axis convention (flipped) instead of XYZ.
    #[serde(default)]
    pub tms: bool,
    #[serde(default = "default_max_zoom")]
    pub max_zoom: u8,
}

fn default_max_zoom() -> u8 {
    19
}

/// Startup position and default provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct General {
    pub start_lat: f64,
    pub start_lon: f64,
    pub start_zoom: u8,
    pub default_provider: String,
    /// Mouse-wheel zoom sensitivity: scroll deltas are scaled by this before
    /// accumulating toward a one-level zoom step. Lower = less sensitive.
    #[serde(default = "default_scroll_sensitivity")]
    pub scroll_sensitivity: f64,
}

fn default_scroll_sensitivity() -> f64 {
    1.0
}

/// On-disk cache limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachePolicy {
    /// Cache directory; defaults to the XDG cache dir when omitted.
    #[serde(default)]
    pub directory: Option<String>,
    #[serde(default = "default_max_size_mb")]
    pub max_size_mb: u64,
    #[serde(default = "default_max_age_days")]
    pub max_age_days: u64,
}

fn default_max_size_mb() -> u64 {
    500
}
fn default_max_age_days() -> u64 {
    30
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self { directory: None, max_size_mb: 500, max_age_days: 30 }
    }
}

/// The full configuration document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub general: General,
    pub providers: Vec<Provider>,
    #[serde(default)]
    pub cache: CachePolicy,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            general: General {
                start_lat: 48.8566,
                start_lon: 2.3522,
                start_zoom: 12,
                default_provider: "OpenStreetMap".into(),
                scroll_sensitivity: 1.0,
            },
            providers: vec![
                Provider {
                    name: "OpenStreetMap".into(),
                    url: "https://tile.openstreetmap.org/{z}/{x}/{y}.png".into(),
                    tms: false,
                    max_zoom: 19,
                },
                Provider {
                    name: "OpenTopoMap".into(),
                    url: "https://a.tile.opentopomap.org/{z}/{x}/{y}.png".into(),
                    tms: false,
                    max_zoom: 17,
                },
            ],
            cache: CachePolicy::default(),
        }
    }
}

impl Config {
    /// Default config path: `~/.config/waltzgps/config.toml`.
    pub fn default_path() -> PathBuf {
        config_home().join("waltzgps").join("config.toml")
    }

    /// Load config from `path`, writing a default file if none exists.
    pub fn load(path: &Path) -> Result<Config> {
        if path.exists() {
            let text = std::fs::read_to_string(path)?;
            let cfg: Config = toml::from_str(&text)?;
            if cfg.providers.is_empty() {
                return Err("config has no providers".into());
            }
            Ok(cfg)
        } else {
            let cfg = Config::default();
            cfg.save(path)?;
            eprintln!("Wrote default config to {}", path.display());
            Ok(cfg)
        }
    }

    /// Serialise the config to `path`, creating parent directories.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Index of the configured default provider (falls back to 0).
    pub fn default_provider_index(&self) -> usize {
        self.providers
            .iter()
            .position(|p| p.name == self.general.default_provider)
            .unwrap_or(0)
    }
}

/// `$XDG_CONFIG_HOME` or `~/.config`.
pub fn config_home() -> PathBuf {
    base_dir("XDG_CONFIG_HOME", ".config")
}

/// `$XDG_CACHE_HOME` or `~/.cache`.
pub fn cache_home() -> PathBuf {
    base_dir("XDG_CACHE_HOME", ".cache")
}

/// `$XDG_STATE_HOME` or `~/.local/state` (for transient state like last view).
pub fn state_home() -> PathBuf {
    base_dir("XDG_STATE_HOME", ".local/state")
}

fn base_dir(env: &str, fallback: &str) -> PathBuf {
    if let Some(val) = std::env::var_os(env) {
        if !val.is_empty() {
            return PathBuf::from(val);
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(fallback)
}
