//! Panel-local UI state on the browser's localStorage: `filters` / `collapsed`
//! survive reload. The sort toggle is gone with the panel's sort button (the
//! tree is always newest-first) and the text filter is deliberately not
//! persisted — a filter you cannot see is a view you cannot explain.
//!
//! Two keys of UI state are a CACHE, not a dataset: this holds `web_sys::Storage`
//! directly and writes fixed-width big-endian bytes as hex. It used to go through
//! `okm_core::LocalStorageStore`, and that backend was withdrawn (ADR-0028 §Withdrawal)
//! for exactly this reason — an engine that implements a whole `VirtualStorage` is the
//! wrong tool for two blobs, and `okm-core`'s dependency closure carries no browser API.
//! The byte shapes are unchanged, so the decode helpers below are the same ones. The old
//! engine left its entries base64-encoded under base64 KEY names (`ui/filters` was stored
//! as `dWkvZmlsdGVycw==`), so the first read of a key adopts what it finds there, rewrites
//! it in this file's shape and drops the old entry — a cache that keeps two encodings of
//! the same fact drifts apart.
//!
//! The keyspace is the panel origin's (`127.0.0.1:9299`), prefixed `ui/` so it never
//! collides with anything else on that origin.
//!
//! Every operation is best-effort: no store (native tests, storage blocked by the
//! browser) reads as defaults and writes are dropped — the panel never breaks on its
//! local cache, same parity rule as the thumbnails config read.

const K_FILTERS: &str = "ui/filters";
const K_COLLAPSED: &str = "ui/collapsed";

/// The same two names as the withdrawn engine wrote them — standard base64 of the key
/// bytes, which is what `b64_encode(b"ui/filters")` produced. Spelled out as constants so
/// the adoption path needs no encoder; the test below holds them to the derivation.
const LEGACY_FILTERS: &str = "dWkvZmlsdGVycw==";
const LEGACY_COLLAPSED: &str = "dWkvY29sbGFwc2Vk";

/// `Option<Storage>` folded at open: wasm gets the real store when the browser
/// hands one over; every other build (native tests) and every blocked context
/// runs stateless.
#[derive(Clone)]
pub struct UiState {
    store: Option<web_sys::Storage>,
}

impl UiState {
    pub fn open() -> Self {
        // The ONE `cfg` split in this file: off wasm, `web_sys::window()` is a stub that
        // panics rather than answering `None`, so it cannot be called at all. Everything
        // else (`set_item` / `get_item`, the hex helpers) compiles on every target and is
        // therefore checked by `cargo test`; only this line needs a wasm toolchain.
        #[cfg(target_arch = "wasm32")]
        let store = web_sys::window().and_then(|w| w.local_storage().ok().flatten());
        #[cfg(not(target_arch = "wasm32"))]
        let store: Option<web_sys::Storage> = None;
        Self { store }
    }

    fn put(&self, key: &str, value: Vec<u8>) {
        let Some(s) = &self.store else { return };
        // The write side is fallible too (ADR-0039): the cache degrades the same way a
        // failed read does — never panics, never breaks the view, and says the refusal
        // on the console instead of dropping it.
        if let Err(e) = s.set_item(key, &hex_encode(&value)) {
            web_sys::console::warn_1(&format!("ui-state write: {e:?}").into());
        }
    }

    fn get(&self, key: &str) -> Option<Vec<u8>> {
        let s = self.store.as_ref()?;
        // Absent (`Ok(None)`) is "never saved" and the defaults win; a refusal is a
        // different fact — the cache still degrades, but it is said on the console
        // instead of being read as an absent key.
        match s.get_item(key) {
            Ok(Some(text)) => hex_decode(&text),
            Ok(None) => adopt(s, key),
            Err(e) => {
                web_sys::console::warn_1(&format!("ui-state read: {e:?}").into());
                None
            }
        }
    }

    // ---- loads (defaults when absent/garbage: cache never breaks the view) ----

    pub fn filters(&self) -> Vec<u32> {
        self.fixed(self.get(K_FILTERS), 4, |c| {
            u32::from_be_bytes([c[0], c[1], c[2], c[3]])
        })
    }

    pub fn collapsed(&self) -> Vec<u64> {
        self.fixed(self.get(K_COLLAPSED), 8, |c| {
            // `fixed` hands out chunks of exactly `width`; a short chunk reads
            // as zero rather than panicking on the conversion.
            u64::from_be_bytes(c.try_into().unwrap_or_default())
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

/// Take over an entry the withdrawn engine wrote: base64 value, base64 KEY. One read,
/// one forward write in this file's shape, one removal — and if any step refuses, the
/// bytes still answer this call (the cache degrades to "readable but not yet moved").
fn adopt(s: &web_sys::Storage, key: &str) -> Option<Vec<u8>> {
    let legacy = legacy_key(key)?;
    let bytes = b64_decode(&s.get_item(legacy).ok().flatten()?)?;
    let _ = s.set_item(key, &hex_encode(&bytes));
    let _ = s.remove_item(legacy);
    Some(bytes)
}

/// Which legacy name to look under — only the two keys this file writes, so a future key
/// cannot silently inherit someone else's encoding.
fn legacy_key(key: &str) -> Option<&'static str> {
    match key {
        K_FILTERS => Some(LEGACY_FILTERS),
        K_COLLAPSED => Some(LEGACY_COLLAPSED),
        _ => None,
    }
}

/// Standard base64 with padding, the alphabet the old engine's keys were built from. Read
/// side only: nothing here writes base64. Rejects bad length, illegal characters and
/// inconsistent padding — a half-decoded key would point at the wrong bytes.
fn b64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let val = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let groups = bytes.len() / 4;
    let mut out = Vec::with_capacity(groups * 3);
    for (i, group) in bytes.chunks(4).enumerate() {
        let pad = usize::from(group[3] == b'=') + usize::from(group[2] == b'=');
        if pad > 0 && (i + 1 != groups || group[3] != b'=') {
            // padding only in the final group, and only as a trailing run
            return None;
        }
        let n = (val(group[0])? << 18)
            | (val(group[1])? << 12)
            | if group[2] == b'=' {
                0
            } else {
                val(group[2])? << 6
            }
            | if group[3] == b'=' { 0 } else { val(group[3])? };
        let chunk = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&chunk[..3 - pad]);
    }
    Some(out)
}

/// Hex, not base64: the values are a handful of bytes, std has no base64, and a
/// hand-rolled alphabet is one more thing that can drift. Lowercase, fixed width.
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The inverse: hex of even length, either case (we write lowercase; a cache read
/// should not care what wrote it). Anything else is refused — a half-decoded cache
/// would be worse than an absent one.
fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Native: no store — every read is the default path, every write a
    /// no-op. The wasm byte shapes are locked by the decode helpers
    /// directly (they are store-independent once the bytes arrive).
    #[test]
    fn the_legacy_names_are_base64_of_the_keys_we_write() {
        assert_eq!(
            b64_decode(LEGACY_FILTERS).as_deref(),
            Some(K_FILTERS.as_bytes())
        );
        assert_eq!(
            b64_decode(LEGACY_COLLAPSED).as_deref(),
            Some(K_COLLAPSED.as_bytes())
        );
        assert!(legacy_key("something/else").is_none());
    }

    #[test]
    fn base64_reads_what_the_old_engine_wrote() {
        // the shapes the panel actually persisted: u32 / u64 big-endian runs
        assert_eq!(
            b64_decode("AAAAAQAAAAI=").as_deref(),
            Some([0, 0, 0, 1, 0, 0, 0, 2].as_slice())
        );
        assert_eq!(b64_decode("").as_deref(), Some([].as_slice()));
        assert_eq!(
            b64_decode("AAA=").as_deref(),
            Some([0, 0].as_slice()),
            "one group, two pads"
        );
        assert!(b64_decode("AA=A").is_none(), "padding not trailing");
        assert!(b64_decode("AAAA====").is_none(), "not a multiple of four");
        assert!(
            b64_decode("AA-A").is_none(),
            "url-safe alphabet is not what we wrote"
        );
    }

    #[test]
    fn hex_round_trips_and_refuses_garbage() {
        for len in 0..6usize {
            let bytes: Vec<u8> = (0..len as u64).map(|i| (i * 37 + 5) as u8).collect();
            assert_eq!(
                hex_decode(&hex_encode(&bytes)).as_deref(),
                Some(bytes.as_slice())
            );
        }
        assert!(hex_decode("abc").is_none(), "odd length is torn");
        assert!(hex_decode("zz").is_none(), "not hex");
        assert_eq!(
            hex_decode("AB").as_deref(),
            Some([0xABu8].as_slice()),
            "reads either case"
        );
    }

    #[test]
    fn stateless_builds_read_defaults() {
        let ui = UiState::open();
        assert!(ui.filters().is_empty());
        assert!(ui.collapsed().is_empty());
        ui.save_filters(&[1, 2]);
        ui.save_collapsed(&[9]);
        assert!(ui.filters().is_empty()); // writes dropped: defaults hold
    }

    #[test]
    fn fixed_width_decode_rejects_torn_values() {
        let ui = UiState::open();
        assert_eq!(
            ui.fixed(Some(vec![0, 0, 0, 1, 0, 0, 0, 2]), 4, |c| {
                u32::from_be_bytes(c.try_into().unwrap())
            }),
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
