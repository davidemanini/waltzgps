//! Background tile fetching.
//!
//! A small pool of worker threads pulls tile requests off a shared queue,
//! serves them from the disk cache when possible, otherwise downloads over HTTP
//! (via the system `curl` binary), and sends the raw bytes back to the GTK main
//! loop for decoding. Using `curl` avoids pulling in an HTTP/TLS crate.
//!
//! Providers may require an API key (see [`crate::config::ApiKeySource`]).
//! Resolving it (reading a file, running a command) is decoupled from the
//! fetch worker pool: it runs on its own one-off thread, at most one
//! resolution in flight per provider at a time, so a slow/hanging command
//! can never starve the fetch workers of threads (cache hits and other
//! providers' tiles keep flowing while a key is being resolved). Fetch
//! workers only ever *read* the memoized result; see [`resolve_key`].

use crate::cache::Cache;
use crate::config::{ApiKeySource, Provider};
use crate::tile::TileId;
use futures_channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const USER_AGENT: &str = concat!("WaltzGPS/", env!("CARGO_PKG_VERSION"), " (map viewer)");
const NUM_WORKERS: usize = 4;
const TIMEOUT_SECS: &str = "20";
/// How long to wait before retrying a provider whose API key failed to
/// resolve (e.g. the file doesn't exist yet, or the command isn't ready).
/// A successful resolution is cached indefinitely (until the provider's
/// config changes), so this backoff only guards the failure path.
const KEY_FAILED_BACKOFF: Duration = Duration::from_secs(15);

/// Identifies a tile *and* the provider it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub provider: usize,
    pub tile: TileId,
}

/// A finished request. `data` is `None` when the fetch failed.
pub struct TileResult {
    pub key: TileKey,
    pub data: Option<Vec<u8>>,
}

/// A multi-consumer work queue with blocking `pop`.
///
/// Serves the **most recently pushed** tile first (LIFO). Because the draw code
/// enqueues exactly the tiles the current view needs, LIFO ensures a freshly
/// panned/zoomed view is served ahead of requests left over from where the user
/// just was.
struct WorkQueue {
    items: Mutex<VecDeque<TileKey>>,
    available: Condvar,
}

impl WorkQueue {
    fn new() -> Self {
        Self { items: Mutex::new(VecDeque::new()), available: Condvar::new() }
    }

    fn push(&self, key: TileKey) {
        self.items.lock().unwrap().push_back(key);
        self.available.notify_one();
    }

    fn pop(&self) -> TileKey {
        let mut items = self.items.lock().unwrap();
        loop {
            // LIFO: newest request (top of stack) first.
            if let Some(key) = items.pop_back() {
                return key;
            }
            items = self.available.wait(items).unwrap();
        }
    }

    /// Drop all not-yet-started requests (e.g. after a zoom/provider change).
    fn clear(&self) {
        self.items.lock().unwrap().clear();
    }
}

/// Outcome of a provider's API-key resolution, memoized so a slow file
/// read or command isn't repeated for every single tile request.
enum ResolvedKeyState {
    /// A dedicated one-off thread is currently resolving this provider's
    /// key. Set *before* that thread is spawned (both under the same lock
    /// acquisition) so that concurrent cache misses across the fetch workers
    /// never spawn more than one resolution attempt per provider.
    Pending,
    /// Cached indefinitely: once a key resolves, it's assumed valid until
    /// the provider's config itself changes (see [`Downloader::set_providers`]).
    Ready(String),
    /// Retried after [`KEY_FAILED_BACKOFF`], so a key that becomes available
    /// later (file appears, command starts succeeding) is picked up without
    /// an app restart.
    Failed { at: Instant },
}

enum KeyState {
    /// The provider has no `api_key_source`; nothing to substitute.
    NotNeeded,
    Ready(String),
    /// Resolution failed (or is on backoff); the caller must not invoke curl.
    Failed,
}

/// Handle used by the UI to enqueue requests and drain finished tiles.
pub struct Downloader {
    queue: Arc<WorkQueue>,
    /// Provider list shared with the workers; updated live on config edits.
    providers: Arc<Mutex<Vec<Provider>>>,
    /// Memoized API-key resolution per provider index; cleared whenever the
    /// provider list changes since indices/sources/values may differ.
    key_cache: Arc<Mutex<HashMap<usize, ResolvedKeyState>>>,
    /// Receiver of finished fetches, taken once by the UI's result pump.
    results: RefCell<Option<UnboundedReceiver<TileResult>>>,
    /// Disk cache shared with the workers; exposed so the UI can update its
    /// policy live (see [`Cache::update_policy`]).
    cache: Arc<Cache>,
}

impl Downloader {
    /// Spawn the worker pool. The returned handle owns the result receiver,
    /// which the UI claims via [`Downloader::take_results`].
    pub fn new(providers: Vec<Provider>, cache: Arc<Cache>) -> Downloader {
        let queue = Arc::new(WorkQueue::new());
        let providers = Arc::new(Mutex::new(providers));
        let key_cache = Arc::new(Mutex::new(HashMap::new()));
        let (res_tx, res_rx) = mpsc::unbounded::<TileResult>();

        for _ in 0..NUM_WORKERS {
            let queue = queue.clone();
            let res_tx: UnboundedSender<TileResult> = res_tx.clone();
            let providers = providers.clone();
            let key_cache = key_cache.clone();
            let cache = cache.clone();
            std::thread::spawn(move || loop {
                let key = queue.pop();
                // Copy the provider out and release the lock before any network
                // I/O so config edits never block on a download.
                let provider = providers.lock().unwrap().get(key.provider).cloned();
                let data = match provider {
                    None => None,
                    Some(provider) => match cache.get(&provider.name, key.tile) {
                        // Cache hit: never needs the API key at all, regardless
                        // of whether it's configured, unresolved, or failing.
                        Some(bytes) => Some(bytes),
                        // Never blocks: a `File`/`Command` resolution in
                        // progress (or on backoff) reports `Failed`
                        // immediately so this worker thread stays free to
                        // serve the next queued tile — including cache hits
                        // for a different provider — instead of stalling.
                        None => match resolve_key(&key_cache, key.provider, &provider) {
                            KeyState::Failed => None, // skip curl entirely
                            KeyState::NotNeeded => {
                                fetch_and_cache(&cache, &provider, key.tile, None)
                            }
                            KeyState::Ready(k) => {
                                fetch_and_cache(&cache, &provider, key.tile, Some(&k))
                            }
                        },
                    },
                };
                if res_tx.unbounded_send(TileResult { key, data }).is_err() {
                    break; // UI gone
                }
            });
        }

        Downloader { queue, providers, key_cache, results: RefCell::new(Some(res_rx)), cache }
    }

    /// Enqueue a tile for fetching (returns immediately).
    pub fn request(&self, key: TileKey) {
        self.queue.push(key);
    }

    /// The shared disk cache, so callers can update its policy live.
    pub fn cache(&self) -> Arc<Cache> {
        self.cache.clone()
    }

    /// Replace the worker pool's provider list (after the config was edited).
    pub fn set_providers(&self, providers: Vec<Provider>) {
        *self.providers.lock().unwrap() = providers;
        // Indices/sources/values may have changed; stale resolved keys must
        // not leak onto whatever provider now sits at the same index.
        self.key_cache.lock().unwrap().clear();
    }

    /// Drop all pending (not-yet-started) requests; used when the view changes
    /// enough that queued tiles are no longer wanted.
    pub fn clear_queue(&self) {
        self.queue.clear();
    }

    /// Claim the result receiver. Panics if called more than once.
    pub fn take_results(&self) -> UnboundedReceiver<TileResult> {
        self.results
            .borrow_mut()
            .take()
            .expect("Downloader::take_results called more than once")
    }
}

/// Download a tile and, on success, store it in the cache. Shared by both
/// the "no key needed" and "key resolved" branches of the worker loop.
fn fetch_and_cache(
    cache: &Cache,
    provider: &Provider,
    tile: TileId,
    api_key: Option<&str>,
) -> Option<Vec<u8>> {
    let bytes = fetch(provider, tile, api_key)?;
    cache.put(&provider.name, tile, &bytes);
    Some(bytes)
}

/// Download a single tile with `curl`, returning its raw bytes.
fn fetch(provider: &Provider, tile: TileId, api_key: Option<&str>) -> Option<Vec<u8>> {
    let url = tile.url(provider, api_key);
    let mut cmd = Command::new("curl");
    cmd.args([
        "-s", // silent
        "-S", // but show errors
        "-L", // follow redirects
        "--max-time",
        TIMEOUT_SECS,
        "-A",
        USER_AGENT,
    ]);
    for header in &provider.headers {
        cmd.arg("-H").arg(format!("{}: {}", header.name, header.value));
    }
    cmd.arg("--").arg(&url);
    let output = cmd.output().ok()?;

    if !output.status.success() || output.stdout.is_empty() {
        // Log the template, not the substituted URL, so a resolved API key
        // embedded in a query string never ends up in logs.
        eprintln!("tile {:?} fetch failed for {}", tile, provider.url);
        return None;
    }
    Some(output.stdout)
}

/// Resolve (and memoize) a provider's API key. Never blocks the caller: a
/// `Literal` source is trivial and answered inline, but `File`/`Command`
/// sources — which can be slow or hang outright — are resolved on a
/// dedicated one-off thread, decoupled from the fetch worker pool, so a
/// stuck command can never starve the workers of threads needed to serve
/// cache hits or other providers' tiles. At most one such thread is ever
/// in flight per provider at a time (see [`ResolvedKeyState::Pending`]); a
/// caller that finds a resolution already pending, or a source that isn't
/// ready yet, gets [`KeyState::Failed`] and must not invoke curl — the tile
/// is retried (and may then see [`KeyState::Ready`]) the next time it's
/// requested. A successful resolution is cached indefinitely; a failed one
/// is retried after [`KEY_FAILED_BACKOFF`].
fn resolve_key(
    key_cache: &Arc<Mutex<HashMap<usize, ResolvedKeyState>>>,
    idx: usize,
    provider: &Provider,
) -> KeyState {
    let Some(src) = &provider.api_key_source else {
        return KeyState::NotNeeded;
    };

    // Can't block or hang, so just answer inline; never touches the cache.
    if let ApiKeySource::Literal(s) = src {
        let key = s.trim().to_string();
        return if key.is_empty() { KeyState::Failed } else { KeyState::Ready(key) };
    }

    let mut guard = key_cache.lock().unwrap();
    match guard.get(&idx) {
        Some(ResolvedKeyState::Ready(key)) => return KeyState::Ready(key.clone()),
        Some(ResolvedKeyState::Pending) => return KeyState::Failed,
        Some(ResolvedKeyState::Failed { at }) if at.elapsed() < KEY_FAILED_BACKOFF => {
            return KeyState::Failed;
        }
        Some(ResolvedKeyState::Failed { .. }) | None => {} // start a fresh resolution below
    }
    // Claim it before releasing the lock, so a concurrent cache miss on
    // another worker sees `Pending` and doesn't start a second, redundant
    // resolution (this is what keeps a slow command to a single process).
    guard.insert(idx, ResolvedKeyState::Pending);
    drop(guard);

    let src = src.clone();
    let key_cache = Arc::clone(key_cache);
    std::thread::spawn(move || {
        let resolved = read_key_source(&src);
        let state = match resolved {
            Some(key) => ResolvedKeyState::Ready(key),
            None => ResolvedKeyState::Failed { at: Instant::now() },
        };
        key_cache.lock().unwrap().insert(idx, state);
    });
    KeyState::Failed
}

/// Read a raw API-key value from its source. Returns `None` (never an
/// empty string) on any failure: missing file, non-zero/unreadable command,
/// or blank content.
fn read_key_source(src: &ApiKeySource) -> Option<String> {
    let raw = match src {
        ApiKeySource::Literal(s) => Some(s.clone()),
        ApiKeySource::File(path) => std::fs::read_to_string(path).ok(),
        ApiKeySource::Command(cmd) => Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok()),
    };
    raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_literal_source() {
        assert_eq!(
            read_key_source(&ApiKeySource::Literal("  secret  ".into())),
            Some("secret".into())
        );
    }

    #[test]
    fn reads_file_source() {
        let path = std::env::temp_dir()
            .join(format!("waltzgps-test-key-{}-{}.txt", std::process::id(), line!()));
        std::fs::write(&path, "file-secret\n").unwrap();
        assert_eq!(read_key_source(&ApiKeySource::File(path.clone())), Some("file-secret".into()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_source_fails() {
        let path = std::env::temp_dir().join("waltzgps-test-key-does-not-exist.txt");
        assert_eq!(read_key_source(&ApiKeySource::File(path)), None);
    }

    #[test]
    fn reads_command_source() {
        assert_eq!(
            read_key_source(&ApiKeySource::Command("echo cmd-secret".into())),
            Some("cmd-secret".into())
        );
    }

    #[test]
    fn failing_command_source_fails() {
        assert_eq!(read_key_source(&ApiKeySource::Command("exit 1".into())), None);
    }

    #[test]
    fn blank_output_counts_as_failure() {
        assert_eq!(read_key_source(&ApiKeySource::Command("printf ''".into())), None);
    }

    fn provider_with(source: ApiKeySource) -> Provider {
        Provider {
            name: "t".into(),
            url: "https://s/{z}/{x}/{y}.png".into(),
            api_key_source: Some(source),
            ..Provider::default()
        }
    }

    /// Poll `resolve_key` until the background resolution thread it kicked
    /// off finishes, or panic after `deadline`. Mirrors how a real caller
    /// (a fetch worker re-requesting the same tile) would eventually see
    /// the result — `resolve_key` itself never blocks.
    fn wait_for_resolution(
        key_cache: &Arc<Mutex<HashMap<usize, ResolvedKeyState>>>,
        idx: usize,
        provider: &Provider,
    ) -> KeyState {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match resolve_key(key_cache, idx, provider) {
                KeyState::Failed if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                other => return other,
            }
        }
    }

    #[test]
    fn resolve_key_resolves_command_asynchronously_and_caches_indefinitely() {
        let counter =
            std::env::temp_dir().join(format!("waltzgps-test-counter-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&counter);
        let provider = provider_with(ApiKeySource::Command(format!(
            "echo -n x >> {} && echo resolved-key",
            counter.display()
        )));
        let key_cache: Arc<Mutex<HashMap<usize, ResolvedKeyState>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // The first call must not block on the command; it kicks off
        // resolution in the background and reports "not ready yet".
        assert!(matches!(resolve_key(&key_cache, 0, &provider), KeyState::Failed));

        match wait_for_resolution(&key_cache, 0, &provider) {
            KeyState::Ready(k) => assert_eq!(k, "resolved-key"),
            _ => panic!("expected Ready once the background resolution finished"),
        }

        // Further calls must be served from the cache, not rerun the command.
        for _ in 0..3 {
            match resolve_key(&key_cache, 0, &provider) {
                KeyState::Ready(k) => assert_eq!(k, "resolved-key"),
                _ => panic!("expected Ready"),
            }
        }
        let runs = std::fs::read_to_string(&counter).unwrap_or_default();
        assert_eq!(runs, "x", "command should be memoized, not rerun per call");
        let _ = std::fs::remove_file(&counter);
    }

    #[test]
    fn resolve_key_deduplicates_concurrent_resolution() {
        // Simulates several fetch workers hitting a cache miss for the same
        // provider at (almost) the same instant, e.g. right after panning to
        // a view whose tiles all belong to one not-yet-resolved provider.
        let counter =
            std::env::temp_dir().join(format!("waltzgps-test-dedup-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&counter);
        let provider = Arc::new(provider_with(ApiKeySource::Command(format!(
            "echo -n x >> {} && sleep 0.2 && echo resolved-key",
            counter.display()
        ))));
        let key_cache: Arc<Mutex<HashMap<usize, ResolvedKeyState>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let handles: Vec<_> = (0..NUM_WORKERS)
            .map(|_| {
                let key_cache = key_cache.clone();
                let provider = provider.clone();
                std::thread::spawn(move || resolve_key(&key_cache, 0, &provider))
            })
            .collect();
        for h in handles {
            // None of these calls may block on the command itself.
            h.join().unwrap();
        }

        match wait_for_resolution(&key_cache, 0, &provider) {
            KeyState::Ready(k) => assert_eq!(k, "resolved-key"),
            _ => panic!("expected Ready once the background resolution finished"),
        }
        let runs = std::fs::read_to_string(&counter).unwrap_or_default();
        assert_eq!(
            runs, "x",
            "command must run exactly once even under concurrent cache misses"
        );
        let _ = std::fs::remove_file(&counter);
    }

    #[test]
    fn resolve_key_retries_failure_only_after_backoff() {
        let path = std::env::temp_dir()
            .join(format!("waltzgps-test-late-key-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let provider = provider_with(ApiKeySource::File(path.clone()));
        let key_cache: Arc<Mutex<HashMap<usize, ResolvedKeyState>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // A recent failure should be reused as-is (no new resolution
        // attempt), even though the file now exists and would succeed.
        std::fs::write(&path, "now-available").unwrap();
        key_cache.lock().unwrap().insert(0, ResolvedKeyState::Failed { at: Instant::now() });
        assert!(matches!(resolve_key(&key_cache, 0, &provider), KeyState::Failed));

        // A failure from beyond the backoff window must be retried.
        key_cache.lock().unwrap().insert(
            0,
            ResolvedKeyState::Failed { at: Instant::now() - KEY_FAILED_BACKOFF - Duration::from_secs(1) },
        );
        match wait_for_resolution(&key_cache, 0, &provider) {
            KeyState::Ready(k) => assert_eq!(k, "now-available"),
            _ => panic!("expected Ready after backoff elapsed"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn resolve_key_not_needed_without_source() {
        let provider = Provider {
            name: "t".into(),
            url: "https://s/{z}/{x}/{y}.png".into(),
            ..Provider::default()
        };
        let key_cache: Arc<Mutex<HashMap<usize, ResolvedKeyState>>> =
            Arc::new(Mutex::new(HashMap::new()));
        assert!(matches!(resolve_key(&key_cache, 0, &provider), KeyState::NotNeeded));
    }
}
