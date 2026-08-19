//! On-disk tile cache with size- and age-based eviction.
//!
//! Layout: `<dir>/<provider>/<z>/<x>/<y>.png`. Safe to share across threads
//! behind an `Arc` — the policy (directory/size/age) lives behind a `Mutex`
//! so it can be swapped live via [`Cache::update_policy`] while workers are
//! reading it; every method briefly locks to snapshot the policy, then does
//! its filesystem I/O without holding the lock.

use crate::config::CachePolicy;
use crate::tile::TileId;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

struct CacheInner {
    dir: PathBuf,
    max_size: u64,
    max_age: Option<Duration>,
}

impl CacheInner {
    fn from_policy(policy: &CachePolicy) -> Self {
        let dir = match &policy.directory {
            Some(d) => expand_tilde(d),
            None => crate::config::cache_home().join("waltzgps"),
        };
        let max_age = if policy.max_age_days == 0 {
            None
        } else {
            Some(Duration::from_secs(policy.max_age_days * 24 * 60 * 60))
        };
        CacheInner { dir, max_size: policy.max_size_mb.saturating_mul(1024 * 1024), max_age }
    }
}

pub struct Cache {
    inner: Mutex<CacheInner>,
}

impl Cache {
    /// Build a cache from policy, resolving the directory (XDG default when
    /// the policy does not specify one). A leading `~/` is expanded.
    pub fn new(policy: &CachePolicy) -> Self {
        Cache { inner: Mutex::new(CacheInner::from_policy(policy)) }
    }

    /// Replace the live policy (directory/size/age limits). Takes effect on
    /// the next cache access. Note: switching directories does not migrate
    /// any files already cached under the old directory — they are simply
    /// left in place, unreferenced.
    pub fn update_policy(&self, policy: &CachePolicy) {
        *self.inner.lock().unwrap() = CacheInner::from_policy(policy);
    }

    /// Snapshot the current directory/size/age so callers can do filesystem
    /// I/O without holding the policy lock.
    fn snapshot(&self) -> (PathBuf, u64, Option<Duration>) {
        let inner = self.inner.lock().unwrap();
        (inner.dir.clone(), inner.max_size, inner.max_age)
    }

    fn tile_path(dir: &Path, provider: &str, tile: TileId) -> PathBuf {
        dir.join(sanitize(provider))
            .join(tile.z.to_string())
            .join(tile.x.to_string())
            .join(format!("{}.png", tile.y))
    }

    /// Return cached bytes for a tile if present and not expired.
    pub fn get(&self, provider: &str, tile: TileId) -> Option<Vec<u8>> {
        let (dir, _, max_age) = self.snapshot();
        let path = Self::tile_path(&dir, provider, tile);
        let meta = std::fs::metadata(&path).ok()?;
        if let Some(max_age) = max_age {
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
        let (dir, _, _) = self.snapshot();
        let path = Self::tile_path(&dir, provider, tile);
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
        let (dir, max_size, max_age) = self.snapshot();
        if !dir.exists() {
            return;
        }
        let mut files: Vec<(PathBuf, u64, SystemTime)> = Vec::new();
        collect_files(&dir, &mut files);

        // Age eviction.
        if let Some(max_age) = max_age {
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
        if max_size > 0 && total > max_size {
            files.sort_by_key(|(_, _, mtime)| *mtime);
            for (path, size, _) in &files {
                if total <= max_size {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(dir: &Path) -> CachePolicy {
        CachePolicy {
            directory: Some(dir.to_string_lossy().into_owned()),
            max_size_mb: 500,
            max_age_days: 30,
        }
    }

    #[test]
    fn update_policy_switches_directory_without_touching_old_files() {
        let base = std::env::temp_dir().join(format!("waltzgps-cache-test-{}", std::process::id()));
        let dir_a = base.join("a");
        let dir_b = base.join("b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();

        let cache = Cache::new(&policy(&dir_a));
        let tile = TileId::new(3, 4, 5);
        cache.put("osm", tile, b"tile-in-a");
        assert_eq!(cache.get("osm", tile), Some(b"tile-in-a".to_vec()));

        cache.update_policy(&policy(&dir_b));

        // The new directory doesn't have this tile yet.
        assert_eq!(cache.get("osm", tile), None);
        cache.put("osm", tile, b"tile-in-b");
        assert_eq!(cache.get("osm", tile), Some(b"tile-in-b".to_vec()));

        // The old directory's file is left untouched, not migrated or deleted.
        let old_path = Cache::tile_path(&dir_a, "osm", tile);
        assert_eq!(std::fs::read(&old_path).unwrap(), b"tile-in-a");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn update_policy_changes_age_limit_used_by_enforce_policy() {
        let dir = std::env::temp_dir().join(format!("waltzgps-cache-test-age-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let cache = Cache::new(&policy(&dir));
        let tile = TileId::new(1, 1, 1);
        cache.put("osm", tile, b"data");
        assert!(cache.get("osm", tile).is_some());

        // Flip to a policy with "never expire" (0 = never), so the tile must
        // survive enforce_policy even though it's not brand new.
        let mut never_expire = policy(&dir);
        never_expire.max_age_days = 0;
        cache.update_policy(&never_expire);
        cache.enforce_policy();
        let path = Cache::tile_path(&dir, "osm", tile);
        assert!(path.exists(), "tile should survive with max_age_days = 0");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
