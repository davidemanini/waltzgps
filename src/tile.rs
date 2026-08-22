//! Tile identity and URL templating.

use crate::config::Provider;

/// A single map tile identified by zoom / column / row (XYZ convention).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileId {
    pub fn new(z: u8, x: u32, y: u32) -> Self {
        Self { z, x, y }
    }

    /// Fill the provider's URL template with this tile's coordinates.
    ///
    /// Supports `{z}`/`{x}`/`{y}`/`{api_key}` placeholders. When the provider
    /// is a TMS source (`tms = true`) the Y axis is flipped relative to
    /// XYZ/slippy-map. `api_key` is the already-resolved key value (or
    /// `None` for providers that don't need one); callers must resolve it
    /// themselves before building the URL.
    pub fn url(&self, provider: &Provider, api_key: Option<&str>) -> String {
        let y = if provider.tms {
            (1u32 << self.z) - 1 - self.y
        } else {
            self.y
        };
        provider
            .url
            .replace("{z}", &self.z.to_string())
            .replace("{x}", &self.x.to_string())
            .replace("{y}", &y.to_string())
            .replace("{api_key}", api_key.unwrap_or(""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(url: &str, tms: bool) -> Provider {
        Provider { name: "t".into(), url: url.into(), tms, max_zoom: 19, ..Provider::default() }
    }

    #[test]
    fn fills_xyz_placeholders() {
        let t = TileId::new(3, 4, 5);
        assert_eq!(
            t.url(&provider("https://s/{z}/{x}/{y}.png", false), None),
            "https://s/3/4/5.png"
        );
    }

    #[test]
    fn tms_flips_y() {
        // At z=3 there are 8 rows (0..=7); TMS row for y=5 is 7-5 = 2.
        let t = TileId::new(3, 4, 5);
        assert_eq!(
            t.url(&provider("https://s/{z}/{x}/{y}.png", true), None),
            "https://s/3/4/2.png"
        );
    }

    #[test]
    fn fills_api_key_placeholder() {
        let t = TileId::new(3, 4, 5);
        assert_eq!(
            t.url(
                &provider("https://s/{z}/{x}/{y}.png?key={api_key}", false),
                Some("abc123")
            ),
            "https://s/3/4/5.png?key=abc123"
        );
    }

    #[test]
    fn missing_api_key_substitutes_empty_string() {
        let t = TileId::new(3, 4, 5);
        assert_eq!(
            t.url(&provider("https://s/{z}/{x}/{y}.png?key={api_key}", false), None),
            "https://s/3/4/5.png?key="
        );
    }
}
