//! Panel-local UI state on the browser's localStorage (ADR-0028's
//! consumer half): `sort_new` / `filters` / `collapsed` survive reload.
//!
//! Deliberately the RAW byte API, not a typed `Collection`: this is the
//! documented UI-state role — no indexes, no epoch, no schema on the
//! node. Fixed-width big-endian entries keep the encoding byte-exact
//! (base64 is the engine's own crossing; adding JSON here would be the
//! text-container defect ADR-0018 forbids). The keyspace is the panel
//! origin's (`127.0.0.1:9299`), prefixed `ui/` so it never collides
//! with anything else on that origin.
//!
//! Every operation is best-effort: no store (native tests, storage
//! blocked by the browser) reads as defaults and writes are dropped —
//! the panel never breaks on its local cache, same parity rule as the
//! thumbnails config read.

use okm_core::localstorage_backend::LocalStorageStore;
use okm_core::VirtualStorage;

const K_SORT: &[u8] = b"ui/sort_new";
const K_FILTERS: &[u8] = b"ui/filters";
const K_COLLAPSED: &[u8] = b"ui/collapsed";

/// `Option<LocalStorageStore>` folded at open: wasm gets the real store
/// when the browser hands one over; every other build (native tests)
/// and every blocked context runs stateless.
#[derive(Clone)]
pub struct UiState {
    store: Option<LocalStorageStore>,
}

impl UiState {
    pub fn open() -> Self {
        #[cfg(target_arch = "wasm32")]
        let store = LocalStorageStore::local().ok();
        #[cfg(not(target_arch = "wasm32"))]
        let store: Option<LocalStorageStore> = None;
        Self { store }
    }

    fn put(&self, key: &[u8], value: Vec<u8>) {
        if let Some(s) = &self.store {
            s.put(key.to_vec(), value);
        }
    }

    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.store.as_ref().and_then(|s| s.get(key))
    }

    // ---- loads (defaults when absent/garbage: cache never breaks the view) ----

    pub fn sort_new(&self) -> bool {
        match self.get(K_SORT).as_deref() {
            Some([1]) => true,
            Some([0]) => false, // absent or malformed: the JS default is newest-first
            _ => true,
        }
    }

    pub fn filters(&self) -> Vec<u32> {
        self.fixed(self.get(K_FILTERS), 4, |c| {
            u32::from_be_bytes([c[0], c[1], c[2], c[3]])
        })
    }

    pub fn collapsed(&self) -> Vec<u64> {
        self.fixed(self.get(K_COLLAPSED), 8, |c| {
            u64::from_be_bytes(c.try_into().expect("chunk of 8"))
        })
    }

    fn fixed<T>(&self, bytes: Option<Vec<u8>>, width: usize, dec: impl Fn(&[u8]) -> T) -> Vec<T> {
        let Some(b) = bytes else { return Vec::new() };
        // a torn tail (length not a multiple of the entry width) drops
        // with the whole value — a half-read list is worse than defaults
        if b.len() % width != 0 {
            return Vec::new();
        }
        b.chunks(width).map(&dec).collect()
    }

    // ---- saves (each write site persists its own new value) ----

    pub fn save_sort(&self, newest_first: bool) {
        self.put(K_SORT, vec![newest_first as u8]);
    }

    pub fn save_filters(&self, ids: &[u32]) {
        let mut v = Vec::with_capacity(ids.len() * 4);
        for id in ids {
            v.extend_from_slice(&id.to_be_bytes());
        }
        self.put(K_FILTERS, v);
    }

    pub fn save_collapsed(&self, ids: &[u64]) {
        let mut v = Vec::with_capacity(ids.len() * 8);
        for id in ids {
            v.extend_from_slice(&id.to_be_bytes());
        }
        self.put(K_COLLAPSED, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Native: no store — every read is the default path, every write a
    /// no-op. The wasm byte shapes are locked by the decode helpers
    /// directly (they are store-independent once the bytes arrive).
    #[test]
    fn stateless_builds_read_defaults() {
        let ui = UiState::open();
        assert!(ui.sort_new());
        assert!(ui.filters().is_empty());
        assert!(ui.collapsed().is_empty());
        ui.save_sort(false); // must not panic without a store
        ui.save_filters(&[1, 2]);
        ui.save_collapsed(&[9]);
        assert!(ui.sort_new()); // writes dropped: defaults hold
    }

    #[test]
    fn fixed_width_decode_rejects_torn_values() {
        let ui = UiState::open();
        assert_eq!(
            ui.fixed(Some(vec![0, 0, 0, 1, 0, 0, 0, 2]), 4, |c| u32::from_be_bytes(
                c.try_into().unwrap()
            )),
            vec![1, 2]
        );
        assert!(ui.fixed(Some(vec![0, 0, 0]), 4, |_| 0u32).is_empty()); // 3 bytes: torn tail
        assert!(ui.fixed(None, 4, |_| 0u32).is_empty());
        // u64 width (collapsed page ids): two BE entries round-exact,
        // a torn tail drops whole (the wasm reload path shares this)
        assert_eq!(
            ui.fixed(
                Some(vec![0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 1, 0]),
                8,
                |c| u64::from_be_bytes(c.try_into().unwrap())
            ),
            vec![7, 256]
        );
        assert!(ui.fixed(Some(vec![0u8; 9]), 8, |_| 0u64).is_empty());
    }
}
