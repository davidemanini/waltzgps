# WARNING

This project was mostly vibe-coded.  Use at your own risk.

# WaltzGPS -- Dance around the world!


WaltzGPS is a map viewer for the Linux desktop

## Why not simply opening OpenStreetMap in a browser?

Because:
 * OSM does not cache tiles
 * No website lets you personalize map providers and switch between
   them with one keypress


## Features

 * Personalizable providers
 * Disc cache
 * Support for HiDPI screens
 * Exploits GPU acceleration
 * Implemented in rust

## Build

```bash
sudo apt install curl cargo librust-gtk4-dev
cargo build --release
```


## Usage

Navigate with the mouse and zoom using the mouse wheel.  Pinch to zoom
is supported, as well.

### Keybinding

* `i` and `o`: zoom in and out
* arrows: move around
* `1`, `2`, ... `9`: switch provider

## Warning

Possible breaking changes in configuration syntax.  Please, backup
before upgrading.
