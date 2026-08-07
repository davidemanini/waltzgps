//! On-disk tile cache with size- and age-based eviction.
//!
//! Layout: `<dir>/<provider>/<z>/<x>/<y>.png`. Safe to share across threads
//! behind an `Arc` — every method operates purely on the filesystem.

use crate::config::CachePolicy;
use crate::tile::TileId;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub struct Cache {
    dir: PathBuf,
    max_size: u64,
    max_age: Option<Duration>,
}

impl Cache {
    /// Build a cache from policy, resolving the directory (XDG default when
    /// the policy does not specify one). A leading `~/` is expanded.
    pub fn new(policy: &CachePolicy) -> Self {
        let dir = match &policy.directory {
            Some(d) => expand_tilde(d),
            None => crate::config::cache_home().join("waltzgps"),
        };
        let max_age = if policy.max_age_days == 0 {
            None
        } else {
            Some(Duration::from_secs(policy.max_age_days * 24 * 60 * 60))
        };
        Cache { dir, max_size: policy.max_size_mb.saturating_mul(1024 * 1024), max_age }
    }

    fn tile_path(&self, provider: &str, tile: TileId) -> PathBuf {
        self.dir
            .join(sanitize(provider))
            .join(tile.z.to_string())
            .join(tile.x.to_string())
            .join(format!("{}.png", tile.y))
    }

    /// Return cached bytes for a tile if present and not expired.
    pub fn get(&self, provider: &str, tile: TileId) -> Option<Vec<u8>> {
        let path = self.tile_path(provider, tile);
        let meta = std::fs::metadata(&path).ok()?;
        if let Some(max_age) = self.max_age {
            if let Ok(modified) = meta.modified() {
                if modified.elapsed().unwrap_or(Duration::ZERO) > max_age {
                    let _ = std::fs::remove_file(&path);
                    return None;
                }
            }
        }
        std::fs::read(&path).ok()
    }

    /// Store bytes for a tile, creating parent directories as needed.
    pub fn put(&self, provider: &str, tile: TileId, bytes: &[u8]) {
        let path = self.tile_path(provider, tile);
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        let _ = std::fs::write(&path, bytes);
    }

    /// Enforce age and size limits across the whole cache directory.
    ///
    /// Deletes expired files first, then evicts oldest-by-mtime files until the
    /// total is within `max_size`.
    pub fn enforce_policy(&self) {
        if !self.dir.exists() {
            return;
        }
        let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        collect_files(&self.dir, &mut files);

        // Age eviction.
        if let Some(max_age) = self.max_age {
            files.retain(|(path, _, mtime)| {
                if mtime.elapsed().unwrap_or(Duration::ZERO) > max_age {
                    let _ = std::fs::remove_file(path);
                    false
                } else {
                    true
                }
            });
        }

        // Size eviction (oldest first).
        let mut total: u64 = files.iter().map(|(_, size, _)| *size).sum();
        if self.max_size > 0 && total > self.max_size {
            files.sort_by_key(|(_, _, mtime)| *mtime);
            for (path, size, _) in &files {
                if total <= self.max_size {
                    break;
                }
                if std::fs::remove_file(path).is_ok() {
                    total = total.saturating_sub(*size);
                }
            }
        }
    }
}

/// Recursively gather regular files with their size and mtime.
fn collect_files(dir: &Path, out: &mut Vec<(PathBuf, u64, SystemTime)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(ft) if ft.is_dir() => collect_files(&path, out),
            Ok(ft) if ft.is_file() => {
                if let Ok(meta) = entry.metadata() {
                    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    out.push((path, meta.len(), mtime));
                }
            }
            _ => {}
        }
    }
}

/// Replace path-hostile characters in a provider name.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// Expand a leading `~/` to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}
