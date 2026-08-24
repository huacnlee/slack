//! Bounding what decoded images cost.
//!
//! A transcript shows an avatar per message and a thumbnail per attachment,
//! and GPUI's own cache retains every image it has ever decoded. Over a long
//! session in a busy workspace that is unbounded growth for pictures nobody is
//! looking at any more.
//!
//! This wraps the built-in cache with a least-recently-used table: the same
//! decoding and the same sharing, with an eviction policy. It is the pattern
//! `longbridge-gpui` uses for its ticker icons.

use std::num::NonZeroUsize;
use std::sync::Arc;

use gpui::{
    App, AppContext, Entity, ImageCache, ImageCacheError, RenderImage, Resource,
    RetainAllImageCache, Window,
};
use lru::LruCache;

/// How many decoded images to keep. A screenful of a dense transcript is a few
/// dozen; this leaves room to scroll a long way before anything is evicted.
pub const DEFAULT_CAPACITY: usize = 256;

pub struct LruImageCache {
    inner: Entity<RetainAllImageCache>,
    /// Insertion order, most recently used last. The value is unit: the images
    /// themselves live in `inner`, this only decides who leaves.
    recent: LruCache<Resource, ()>,
}

impl LruImageCache {
    pub fn new(capacity: usize, cx: &mut App) -> Entity<Self> {
        let capacity = NonZeroUsize::new(capacity.max(1)).expect("capacity is at least one");
        let inner = RetainAllImageCache::new(cx);
        cx.new(|_| Self {
            inner,
            recent: LruCache::new(capacity),
        })
    }

    /// Forget everything, for a conversation switch.
    pub fn clear(&mut self, window: &mut Window, cx: &mut App) {
        self.recent.clear();
        self.inner.update(cx, |inner, cx| inner.clear(window, cx));
    }
}

impl ImageCache for LruImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        self.inner.update(cx, |inner, cx| {
            if self.recent.contains(resource) {
                self.recent.promote(resource);
                return inner.load(resource, window, cx);
            }

            // Admitting this one may push the coldest one out.
            if let Some((evicted, _)) = self.recent.push(resource.clone(), ()) {
                inner.remove(&evicted, window, cx);
            }
            inner.load(resource, window, cx)
        })
    }
}
