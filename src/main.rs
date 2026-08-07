//! WaltzGPS — a GTK4 Tile Map Service / Web-Mercator map viewer.

mod app_state;
mod cache;
mod config;
mod downloader;
mod geo;
mod tile;
mod ui;

use crate::app_state::AppState;
use crate::cache::Cache;
use crate::config::Config;
use crate::downloader::Downloader;
use gtk::prelude::*;
use std::rc::Rc;
use std::sync::Arc;

const APP_ID: &str = "org.waltzgps.WaltzGPS";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Resolve config path (`--config <path>` or the XDG default) and load it,
    // writing defaults on first run.
    let config_path = match parse_config_arg() {
        Some(p) => std::path::PathBuf::from(p),
        None => Config::default_path(),
    };
    let config = Rc::new(Config::load(&config_path)?);

    let app = gtk::Application::builder().application_id(APP_ID).build();

    app.connect_activate(move |app| {
        let config = (*config).clone();

        // Disk cache shared with worker threads; trim it in the background.
        let cache = Arc::new(Cache::new(&config.cache));
        {
            let cache = cache.clone();
            std::thread::spawn(move || cache.enforce_policy());
        }

        let providers = Arc::new(config.providers.clone());
        let downloader = Rc::new(Downloader::new(providers, cache));
        let state = AppState::new(config);

        ui::window::build_ui(app, state, downloader);
    });

    // Don't let GTK parse our own CLI arguments.
    app.run_with_args(&["waltzgps"]);
    Ok(())
}

/// Minimal `--config <path>` / `--config=<path>` parser.
fn parse_config_arg() -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(rest) = arg.strip_prefix("--config=") {
            return Some(rest.to_string());
        }
        if arg == "--config" {
            return args.next();
        }
    }
    None
}
