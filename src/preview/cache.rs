#![forbid(unsafe_code)]
//! The cache of prepared images (P3 4.4, 2.6): at most 8 images and 64 MB, least recently
//! used out first. Its key is the entry's `(st_dev, st_ino, mtime, size)` (for a non-local
//! entry, its place and metadata), the pane's cells and pixel size, and the protocol. A hit
//! decodes nothing; a kitty image the terminal still stores is then placed again without a
//! transmit, because the prepared image keeps its id.

use super::card::Card;
use super::gfx::Prepared;
use super::{Pane, Protocol};
use crate::fsops::sys::Ts;
use crate::provider::VPath;
use std::sync::Arc;

/// At most this many prepared images (P3 2.6).
pub const MAX_IMAGES: usize = 8;
/// At most this many bytes of prepared images (P3 2.6).
pub const MAX_BYTES: usize = 64 << 20;

/// Which entry an image shows.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Local {
        dev: u64,
        ino: u64,
        mtime: Ts,
        size: u64,
    },
    Place {
        place: u64,
        path: VPath,
        mtime: i64,
        size: u64,
    },
}

/// The cache key (P3 4.4).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub source: Source,
    pub pane: Pane,
    pub protocol: Protocol,
}

/// A least-recently-used list of prepared images with their cards.
#[derive(Debug, Default)]
pub struct Cache {
    /// Least recently used first.
    items: Vec<(Key, Arc<Prepared>, Card)>,
    bytes: usize,
}

impl Cache {
    /// The image for `key`, which becomes the most recently used.
    pub fn get(&mut self, key: &Key) -> Option<(Arc<Prepared>, Card)> {
        let k = self.items.iter().position(|(k, _, _)| k == key)?;
        let item = self.items.remove(k);
        let out = (item.1.clone(), item.2.clone());
        self.items.push(item);
        Some(out)
    }

    /// Adds an image; the least recently used go until both bounds hold. An image larger
    /// than the whole bound is not kept.
    pub fn insert(&mut self, key: Key, image: Arc<Prepared>, card: Card) {
        if let Some(k) = self.items.iter().position(|(k, _, _)| *k == key) {
            let old = self.items.remove(k);
            self.bytes -= old.1.bytes();
        }
        let size = image.bytes();
        if size > MAX_BYTES {
            return;
        }
        self.bytes += size;
        self.items.push((key, image, card));
        while self.items.len() > MAX_IMAGES || self.bytes > MAX_BYTES {
            let old = self.items.remove(0);
            self.bytes -= old.1.bytes();
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::gfx::Body;

    fn key(n: u64) -> Key {
        Key {
            source: Source::Local {
                dev: 1,
                ino: n,
                mtime: Ts::default(),
                size: 1,
            },
            pane: Pane::default(),
            protocol: Protocol::Kitty,
        }
    }

    fn image(bytes: usize) -> Arc<Prepared> {
        Arc::new(Prepared {
            id: 1,
            pane: (1, 1),
            cells: (1, 1),
            px: (1, 1),
            body: Body::Kitty {
                transmit: vec![0; bytes],
            },
        })
    }

    #[test]
    fn at_most_eight_images_and_the_byte_bound() {
        let mut c = Cache::default();
        for n in 0..10 {
            c.insert(key(n), image(10), Card::default());
        }
        assert_eq!(c.len(), MAX_IMAGES);
        assert!(c.get(&key(0)).is_none() && c.get(&key(1)).is_none());
        // A hit is the most recently used and survives the next insert.
        assert!(c.get(&key(2)).is_some());
        c.insert(key(10), image(10), Card::default());
        assert!(c.get(&key(2)).is_some() && c.get(&key(3)).is_none());
        c.insert(key(11), image(MAX_BYTES / 2), Card::default());
        c.insert(key(12), image(MAX_BYTES / 2), Card::default());
        assert!(c.bytes() <= MAX_BYTES);
        assert!(c.get(&key(11)).is_none(), "the older half went");
        c.insert(key(13), image(MAX_BYTES + 1), Card::default());
        assert!(c.get(&key(13)).is_none());
    }
}
