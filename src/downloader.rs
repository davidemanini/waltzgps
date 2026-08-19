//! Background tile fetching.
//!
//! A small pool of worker threads pulls tile requests off a shared queue,
//! serves them from the disk cache when possible, otherwise downloads over HTTP
//! (via the system `curl` binary), and sends the raw bytes back to the GTK main
//! loop for decoding. Using `curl` avoids pulling in an HTTP/TLS crate.

use crate::cache::Cache;
use crate::config::Provider;
use crate::tile::TileId;
use futures_channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};

const USER_AGENT: &str = concat!("WaltzGPS/", env!("CARGO_PKG_VERSION"), " (map viewer)");
const NUM_WORKERS: usize = 4;
const TIMEOUT_SECS: &str = "20";

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

/// Handle used by the UI to enqueue requests and drain finished tiles.
pub struct Downloader {
    queue: Arc<WorkQueue>,
    /// Provider list shared with the workers; updated live on config edits.
    providers: Arc<Mutex<Vec<Provider>>>,
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
        let (res_tx, res_rx) = mpsc::unbounded::<TileResult>();

        for _ in 0..NUM_WORKERS {
            let queue = queue.clone();
            let res_tx: UnboundedSender<TileResult> = res_tx.clone();
            let providers = providers.clone();
            let cache = cache.clone();
            std::thread::spawn(move || loop {
                let key = queue.pop();
                // Copy the provider out and release the lock before any network
                // I/O so config edits never block on a download.
                let provider = providers.lock().unwrap().get(key.provider).cloned();
                let data = match provider {
                    None => None,
                    Some(provider) => match cache.get(&provider.name, key.tile) {
                        Some(bytes) => Some(bytes),
                        None => match fetch(&provider, key.tile) {
                            Some(bytes) => {
                                cache.put(&provider.name, key.tile, &bytes);
                                Some(bytes)
                            }
                            None => None,
                        },
                    },
                };
                if res_tx.unbounded_send(TileResult { key, data }).is_err() {
                    break; // UI gone
                }
            });
        }

        Downloader { queue, providers, results: RefCell::new(Some(res_rx)), cache }
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

/// Download a single tile with `curl`, returning its raw bytes.
fn fetch(provider: &Provider, tile: TileId) -> Option<Vec<u8>> {
    let url = tile.url(provider);
    let output = Command::new("curl")
        .args([
            "-s",            // silent
            "-S",            // but show errors
            "-L",            // follow redirects
            "--max-time",
            TIMEOUT_SECS,
            "-A",
            USER_AGENT,
            "--",
            &url,
        ])
        .output()
        .ok()?;

    if !output.status.success() || output.stdout.is_empty() {
        eprintln!("tile {:?} fetch failed for {url}", tile);
        return None;
    }
    Some(output.stdout)
}
