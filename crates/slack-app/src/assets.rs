//! Asset source for the window.
//!
//! Product icons ship with this binary; everything else falls through to the
//! shared component icon set, so `IconName` and `SlackIcon` can be used
//! interchangeably in the same tree.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

#[derive(rust_embed::RustEmbed)]
#[folder = "assets"]
#[include = "icons/**/*.svg"]
struct ProductIcons;

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }
        if let Some(file) = ProductIcons::get(path) {
            return Ok(Some(file.data));
        }
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths: Vec<SharedString> = ProductIcons::iter()
            .filter(|p| p.starts_with(path))
            .map(Into::into)
            .collect();
        paths.extend(gpui_component_assets::Assets.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
