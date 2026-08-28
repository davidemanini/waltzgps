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
    /// URL template using `{z}`/`{x}`/`{y}`/`{api_key}` placeholders.
    pub url: String,
    /// When true, use TMS Y-axis convention (flipped) instead of XYZ.
    #[serde(default)]
    pub tms: bool,
    #[serde(default = "default_max_zoom")]
    pub max_zoom: u8,
    /// Where to obtain `{api_key}`'s value, if the URL template uses it.
    #[serde(default)]
    pub api_key_source: Option<ApiKeySource>,
    /// Extra HTTP headers sent with every tile request for this provider.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<Header>,
}

impl Default for Provider {
    fn default() -> Self {
        Provider {
            name: String::new(),
            url: String::new(),
            tms: false,
            max_zoom: default_max_zoom(),
            api_key_source: None,
            headers: Vec::new(),
        }
    }
}

fn default_max_zoom() -> u8 {
    19
}

/// Where a provider's API key comes from. At most one source is active at
/// a time (enforced by construction, since this is an enum rather than
/// three separate optional fields).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ApiKeySource {
    /// Typed directly into the config/UI.
    Literal(String),
    /// Read the entire file contents (trimmed) each time the key is resolved.
    File(PathBuf),
    /// Run via a shell, capture trimmed stdout.
    Command(String),
}

/// A custom HTTP header sent with a provider's tile requests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub name: String,
    pub value: String,
}

/// Startup position and default provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct General {
    /// Mouse-wheel zoom sensitivity: scroll deltas are scaled by this before
    /// accumulating toward a one-level zoom step. Lower = less sensitive.
    #[serde(default = "default_scroll_sensitivity")]
    pub scroll_sensitivity: f64,
    /// Whether trackpad pinch-to-zoom is active at all.
    #[serde(default = "default_pinch_zoom_enabled")]
    pub pinch_zoom_enabled: bool,
    /// Pinch-gesture zoom sensitivity: the gesture's scale delta (in log2
    /// units, i.e. zoom levels) is scaled by this. Lower = less sensitive.
    #[serde(default = "default_pinch_zoom_sensitivity")]
    pub pinch_zoom_sensitivity: f64,
}

fn default_scroll_sensitivity() -> f64 {
    0.1
}

fn default_pinch_zoom_enabled() -> bool {
    true
}

fn default_pinch_zoom_sensitivity() -> f64 {
    1.0
}

/// On-disk cache limits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachePolicy {
    /// Cache directory; defaults to the XDG cache dir when omitted.
    #[serde(default)]
    pub directory: Option<PathBuf>,
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
                scroll_sensitivity: 1.0,
                pinch_zoom_enabled: default_pinch_zoom_enabled(),
                pinch_zoom_sensitivity: default_pinch_zoom_sensitivity(),
            },
            providers: vec![
                Provider {
                    name: "OpenStreetMap".into(),
                    url: "https://tile.openstreetmap.org/{z}/{x}/{y}.png".into(),
                    tms: false,
                    max_zoom: 19,
                    ..Provider::default()
                },
                Provider {
                    name: "OpenTopoMap".into(),
                    url: "https://a.tile.opentopomap.org/{z}/{x}/{y}.png".into(),
                    tms: false,
                    max_zoom: 17,
                    ..Provider::default()
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
            for p in &cfg.providers {
                if let Some(src) = &p.api_key_source {
                    let empty = match src {
                        ApiKeySource::Literal(s) => s.trim().is_empty(),
                        ApiKeySource::File(path) => path.as_os_str().is_empty(),
                        ApiKeySource::Command(cmd) => cmd.trim().is_empty(),
                    };
                    if empty {
                        return Err(
                            format!("provider '{}' has an empty api_key_source value", p.name)
                                .into(),
                        );
                    }
                }
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
    pub fn provider_index(&self, provider: String) -> usize {
        self.providers
            .iter()
            .position(|p| p.name == provider)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_with(source: ApiKeySource) -> Provider {
        Provider {
            name: "t".into(),
            url: "https://s/{z}/{x}/{y}.png?key={api_key}".into(),
            api_key_source: Some(source),
            headers: vec![Header { name: "X-Custom".into(), value: "abc".into() }],
            ..Provider::default()
        }
    }

    fn roundtrip(p: Provider) -> Provider {
        let toml_text = toml::to_string_pretty(&p).unwrap();
        toml::from_str(&toml_text).unwrap()
    }

    #[test]
    fn roundtrips_literal_api_key_source() {
        let p = provider_with(ApiKeySource::Literal("secret".into()));
        let back = roundtrip(p);
        assert!(matches!(back.api_key_source, Some(ApiKeySource::Literal(s)) if s == "secret"));
        assert_eq!(back.headers, vec![Header { name: "X-Custom".into(), value: "abc".into() }]);
    }

    #[test]
    fn roundtrips_file_api_key_source() {
        let p = provider_with(ApiKeySource::File(PathBuf::from("/tmp/key.txt")));
        let back = roundtrip(p);
        assert!(
            matches!(back.api_key_source, Some(ApiKeySource::File(ref path)) if path == Path::new("/tmp/key.txt"))
        );
    }

    #[test]
    fn roundtrips_command_api_key_source() {
        let p = provider_with(ApiKeySource::Command("echo abc".into()));
        let back = roundtrip(p);
        assert!(matches!(back.api_key_source, Some(ApiKeySource::Command(c)) if c == "echo abc"));
    }

    #[test]
    fn provider_without_api_key_source_omits_it() {
        let p = Provider {
            name: "t".into(),
            url: "https://s/{z}/{x}/{y}.png".into(),
            ..Provider::default()
        };
        let back = roundtrip(p);
        assert!(back.api_key_source.is_none());
        assert!(back.headers.is_empty());
    }
}
