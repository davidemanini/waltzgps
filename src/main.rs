//! WaltzGPS — a GTK4 Tile Map Service / Web-Mercator map viewer.

mod app_state;
mod cache;
mod config;
mod downloader;
mod geo;
mod persist;
mod tile;
mod ui;

use crate::app_state::AppState;
use crate::cache::Cache;
use crate::config::Config;
use crate::downloader::Downloader;
use clap::Parser;
use gtk::prelude::*;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

const APP_ID: &str = "org.waltzgps.WaltzGPS";

/// WaltzGPS — a GTK4 Tile Map Service / Web-Mercator map viewer.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Path to the configuration file
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse our own args before GTK gets a chance to consume them.
    let cli = Cli::parse();

    let config_path = cli.config.unwrap_or_else(Config::default_path);
    let config = Rc::new(Config::load(&config_path)?);
    let config_path = Rc::new(config_path);

    let app = gtk::Application::builder().application_id(APP_ID).build();

    app.connect_activate(move |app| {
        let config = (*config).clone();
        let config_path = (*config_path).clone();

        // Restore the last-viewed position/zoom, if any.
        let path = crate::persist::MapPersist::path();
        let saved = crate::persist::MapPersist::load(&path).unwrap_or_default();
//        st.map.zoom = saved.zoom.min(max_zoom);

        // Disk cache shared with worker threads; trim it in the background.
        let cache = Arc::new(Cache::new(&config.cache));
        {
            let cache = cache.clone();
            std::thread::spawn(move || cache.enforce_policy());
        }

        let downloader = Rc::new(Downloader::new(config.providers.clone(), cache));
        let state = AppState::new(config, saved, config_path);


        ui::window::build_ui(app, state, downloader);
    });

    // Don't let GTK parse our own CLI arguments.
    app.run_with_args(&["waltzgps"]);
    Ok(())
}


