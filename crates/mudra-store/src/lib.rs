//! mudra-store — the okm collection layer for mudrad, implementing the
//! layout contract in `docs/SCHEMA.md` (R1 acceptance spec).
//!
//! Namespaces follow the SCHEMA declaration order. The page_tag junction
//! carries no ns of its own: its entries live one in Page's ns 2 and one
//! in Tag's ns 1 (direction = which ns the entry lives in), so ns 3 is a
//! permanent hole (ADR-0002 no-reuse).
//!
//! Payload notes that bind to okm's field whitelist (u8/u16/u32/u64,
//! i8/i16/i32/i64, String, Bytes, [u8;N], Quant, VarInt, Enum, Offset):
//! there is no `bool` and no payload `Option`, so flags are `u8` 0/1 and
//! "absent" timestamps are the `0` sentinel (real epoch-zero is outside
//! the wall-clock domain).
//!
//! Engine note: `FjallStore` clones share the underlying keyspace
//! (Arc-inner), so one `Database` fans out to every `Collection` handle.
//!
//! Feature note: the `engine` feature (default) adds the fjall assembly
//! points and lifecycle. Without it (`default-features = false`, how the
//! wasm panel consumes this crate) the crate is the schema layer only —
//! keys, documents, junction type, index markers, ns constants — every
//! byte-level fact shared single-source with mudrad, no engine types
//! (fjall does not compile for wasm32).

use okm_core::{Bytes, DocumentEncode, JunctionEncode, KeyEncode, Quant, Ref};
#[cfg(feature = "engine")]
use okm_core::{Collection, FjallStore, Junction};

#[cfg(feature = "engine")]
use std::path::Path;

#[cfg(feature = "engine")]
pub mod lifecycle;
#[cfg(feature = "engine")]
pub use lifecycle::{TargetInfo, UpsertMode};

// ================= keys =================

/// Tag identity: surrogate id only (tree position lives on the row).
#[derive(KeyEncode, Clone, PartialEq, Eq, Debug, Default)]
pub struct TagKey {
    pub id: u32,
}

/// Page identity: surrogate id.
#[derive(KeyEncode, Clone, PartialEq, Eq, Debug, Default)]
pub struct PageKey {
    pub id: u64,
}

/// Instance identity: surrogate id (profile is an attribute, not identity).
#[derive(KeyEncode, Clone, PartialEq, Eq, Debug, Default)]
pub struct InstanceKey {
    pub id: u32,
}

/// SiteWidth identity: surrogate id (`site` is indexed, never a key).
#[derive(KeyEncode, Clone, PartialEq, Eq, Debug, Default)]
pub struct SiteWidthKey {
    pub id: u32,
}

/// History identity: surrogate id (`url` is indexed, never key materialized).
#[derive(KeyEncode, Clone, PartialEq, Eq, Debug, Default)]
pub struct HistoryKey {
    pub id: u64,
}

/// State identity: a fixed slot from the closed set in [`state`].
#[derive(KeyEncode, Clone, PartialEq, Eq, Debug, Default)]
pub struct StateKey {
    pub slot: u8,
}

// ================= Tag (ns 1) =================

/// Bare-name addressing is a func index: the query side calls the same
/// path, so normalization cannot drift between write and read.
fn tag_name(tag: &Tag) -> String {
    tag.name.clone()
}

/// A tag-forest node. `parent_id = -1` is the root sentinel (no real
/// parent). Flags are u8 0/1. `rank` orders siblings (`0` = unranked).
#[derive(DocumentEncode, Clone, PartialEq, Eq, Debug, Default)]
#[ok_ref(TagKey)]
#[ok_ns(1)]
#[ok_index(by_parent { fields(parent_id) })]
#[ok_index(by_name { func(tag_name) })]
pub struct Tag {
    pub parent_id: i32,
    pub name: String,
    pub alias: String,
    pub isolated: u8,
    pub required: u8,
    pub hidden: u8,
    pub deleted: u8,
    pub rank: i32,
    pub note: String,
}

// ================= Page (ns 2) =================

/// URL n-gram fan-out (the `okm-ngram` recipe): one row into N gram
/// entries via a multi-value func index. Recall lives here; BM25 rerank
/// is the caller's.
fn url_grams(page: &Page) -> Vec<String> {
    okm_ngram::ngrams(&page.url, 3)
}

/// Target reverse lookup is a func index over the CDP string id
/// (`/open {tabId}` and `focus_page` resolve rows by it).
fn page_target(page: &Page) -> String {
    page.target_id.clone()
}

/// Host substring of a URL — the site-width join key.
pub fn url_site(url: &str) -> String {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    after_scheme.split('/').next().unwrap_or("").to_string()
}

/// An open (or historically closed) browser page.
///
/// Timestamps are u64 wall-clock millis with `0` = never/absent.
/// `deleted_at != 0` is the soft-delete marker; only closed pages may be
/// deleted (an app-layer rule — every read path carries the filter).
#[derive(DocumentEncode, Clone, PartialEq, Eq, Debug, Default)]
#[ok_ref(PageKey)]
#[ok_ns(2)]
#[ok_index(by_instance { fields(instance_id) })]
#[ok_index(by_parent { fields(parent_id) })]
#[ok_index(by_target { func(page_target) })]
#[ok_index(by_url { func(url_grams) })]
pub struct Page {
    pub instance_id: u32,
    pub target_id: String,
    pub url: String,
    pub title: String,
    pub position: u32,
    pub opened_at: u64,
    pub closed_at: u64,
    pub deleted_at: u64,
    pub parent_id: u64,
}

// ================= page_tag junction =================

/// Page↔Tag junction (segment 0x3, discriminator 1). Page side carries
/// the full PageKey identity; Tag side the full TagKey identity.
/// Cross-tree multi-select = multiple links; within-tree single-select is
/// app law.
#[derive(JunctionEncode, Clone, Debug)]
#[ok_junction(1)]
pub struct PageTag {
    pub page: Ref<Page, PageKey>,
    pub tag: Ref<Tag, TagKey>,
}

// ================= Instance (ns 4) =================

fn instance_profile(inst: &Instance) -> String {
    inst.profile.clone()
}

/// A chromium instance bound to one situation leaf. `running` is 0/1;
/// the row is advisory — `/proc` liveness probes are mudrad's job.
#[derive(DocumentEncode, Clone, PartialEq, Eq, Debug, Default)]
#[ok_ref(InstanceKey)]
#[ok_ns(4)]
#[ok_index(by_profile { func(instance_profile) })]
pub struct Instance {
    pub profile: String,
    pub port: u32,
    pub pid: u32,
    pub running: u8,
    pub proxy: String,
    pub extensions: String,
}

// ================= SiteWidth (ns 5) =================

fn site_name(sw: &SiteWidth) -> String {
    sw.site.clone()
}

/// Remembered window width per site. `site` stays a func index (variable
/// length, never key materialized); `proportion` is a 0..1 float on the
/// Quant fixed-point wire.
#[derive(DocumentEncode, Clone, PartialEq, Debug, Default)]
#[ok_ref(SiteWidthKey)]
#[ok_ns(5)]
#[ok_index(by_site { func(site_name) })]
pub struct SiteWidth {
    pub site: String,
    pub proportion: Quant<4>,
}

// ================= State (ns 6) =================

pub mod state {
    //! Closed slot set for the State collection. Slot ids are a
    //! persistent contract — append only, never renumber.
    pub const CURRENT_CONTEXT: u8 = 1;
    pub const WALKER_MODE: u8 = 2;
    pub const OP_MOD: u8 = 3;
    pub const SORT: u8 = 4;
    pub const DEV_MODE: u8 = 5;
    pub const EPOCH: u8 = 6;
    pub const PAGE_ID: u8 = 7;
    pub const TAG_ID: u8 = 8;
    pub const INSTANCE_ID: u8 = 9;
    pub const SITE_WIDTH_ID: u8 = 10;
    pub const PANEL_PID: u8 = 11;
    pub const HISTORY_ID: u8 = 12;
}

/// One opaque raw value per State slot (text settings, u64 counters, the
/// epoch). The slot decides the interpretation; the codec stays generic.
#[derive(DocumentEncode, Clone, PartialEq, Eq, Debug, Default)]
#[ok_ref(StateKey)]
#[ok_ns(6)]
pub struct State {
    pub value: Bytes,
}

// ================= History (ns 7) =================

fn history_url(h: &History) -> String {
    h.url.clone()
}

/// One open-history entry: an address-bar visit accumulator. `visits`
/// counts open-verb calls (no log — RRF ranks on the count directly);
/// `title` carries the last-known title for candidate labels. `url`
/// stays a func index (variable length, never key materialized) so the
/// write path finds the row to bump by exact url without a full scan.
#[derive(DocumentEncode, Clone, PartialEq, Eq, Debug, Default)]
#[ok_ref(HistoryKey)]
#[ok_ns(7)]
#[ok_index(by_url { func(history_url) })]
pub struct History {
    pub url: String,
    pub title: String,
    pub visits: u64,
    pub last_at: u64,
}

/// Address-bar style similarity: subsequence match of `query` over
/// `target` (case-insensitive), rewarding contiguous runs and early
/// matches. Returns 0.0..=1.0; a query that is not a subsequence is 0.
/// Pure function — the ranking lives here so mudrad, tests, and any
/// future consumer share one definition.
pub fn sim_score(query: &str, target: &str) -> f64 {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let t: Vec<char> = target.chars().flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return 0.0;
    }
    if t.is_empty() {
        return 0.0;
    }
    // greedy scan with run/position accounting: find q[] in t[] left-to-
    // right; every matched char scores 1, an extension of the previous
    // contiguous run scores +1 extra, matching at the very start scores
    // +1 extra (prefix bias).
    let mut qi = 0usize;
    let mut score = 0.0f64;
    let mut prev_match: Option<usize> = None;
    for (ti, tc) in t.iter().enumerate() {
        if qi < q.len() && tc == &q[qi] {
            let mut s = 1.0;
            if ti > 0 && prev_match == Some(ti - 1) {
                s += 1.0; // contiguous run bonus
            }
            if ti == qi {
                s += 1.0; // prefix alignment bonus (matched at the head)
            }
            score += s;
            prev_match = Some(ti);
            qi += 1;
        }
    }
    if qi < q.len() {
        return 0.0; // not a subsequence
    }
    // normalize: max achievable is 3 per query char (run + prefix),
    // floor is 1; divide by the span actually consumed so a match
    // crammed at the head beats one scattered over the tail.
    let max = 3.0 * q.len() as f64;
    let span = prev_match.map_or(1, |last| last + 1).max(q.len()) as f64;
    (score / max).min(1.0) * (q.len() as f64 / span).sqrt()
}

/// Reciprocal-rank fusion over two ranked lists (same items, rank 0 =
/// best). k=60 is the standard smoothing constant.
pub fn rrf(rank_a: usize, rank_b: usize, k: f64) -> f64 {
    1.0 / (k + rank_a as f64 + 1.0) + 1.0 / (k + rank_b as f64 + 1.0)
}

/// RRF smoothing constant (standard paper value; also the panel default).
pub const RRF_K: f64 = 60.0;

/// Address-bar completion candidates: similarity x visits fused via RRF.
/// Non-empty query keeps only subsequence hits (sim over url OR title,
/// whichever is stronger); empty query falls back to pure visits order.
/// Pure function over the row snapshot — the ranking law has one home.
pub fn history_candidates(rows: &[History], query: &str, limit: usize) -> Vec<History> {
    let q = query.trim();
    if q.is_empty() {
        let mut all: Vec<History> = rows.to_vec();
        all.sort_by(|a, b| b.visits.cmp(&a.visits).then(b.last_at.cmp(&a.last_at)));
        all.truncate(limit);
        return all;
    }
    let mut hits: Vec<(f64, History)> = rows
        .iter()
        .filter_map(|h| {
            let sim = sim_score(q, &h.url).max(sim_score(q, &h.title));
            (sim > 0.0).then(|| (sim, h.clone()))
        })
        .collect();
    // sim order (ties: stable = row order); index in this vec IS rank_sim.
    hits.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    // visits rank among the same candidate set.
    let mut by_visits: Vec<usize> = (0..hits.len()).collect();
    by_visits.sort_by(|&i, &j| {
        hits[j]
            .1
            .visits
            .cmp(&hits[i].1.visits)
            .then(hits[j].1.last_at.cmp(&hits[i].1.last_at))
    });
    let mut visit_rank = vec![0usize; hits.len()];
    for (r, &i) in by_visits.iter().enumerate() {
        visit_rank[i] = r;
    }
    let mut scored: Vec<(f64, usize)> = hits
        .iter()
        .enumerate()
        .map(|(i, _)| (rrf(i, visit_rank[i], RRF_K), i))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    scored.into_iter().map(|(_, i)| hits[i].1.clone()).collect()
}

// ================= generated index markers =================

// `DocumentEncode` names each access-method marker
// `__OkmIndex_<Row>_<index>`. Aliases give scan call sites readable
// types; `Collection::scan::<I>` takes the marker as a type parameter.
// `pub` so the wasm panel (frame-level scans) addresses the same markers.
pub use __OkmIndex_Instance_by_profile as ByProfile;
pub use __OkmIndex_Page_by_instance as ByInstance;
pub use __OkmIndex_Page_by_parent as ByPageParent;
pub use __OkmIndex_Page_by_target as ByTarget;
pub use __OkmIndex_Page_by_url as ByUrl;
pub use __OkmIndex_History_by_url as ByHistoryUrl;
pub use __OkmIndex_SiteWidth_by_site as BySite;
pub use __OkmIndex_Tag_by_name as ByTagName;
pub use __OkmIndex_Tag_by_parent as ByTagParent;

// ================= engine-free byte layout =================

/// The table's key header, byte-identical to `Collection::header`
/// (engine-free mirror for the wasm panel's frame-level scans):
/// `[0xFF][part 1B] (if declared)[ns 2B]`. A layout-only reader that
/// never opens a `Collection` still addresses the same keyspace —
/// `PARTITION_PREFIX`/`NS_PREFIX` are the derive's single source.
pub fn primary_header<R: okm_core::Document>() -> Vec<u8> {
    let mut buf = Vec::with_capacity(4);
    buf.extend_from_slice(R::PARTITION_PREFIX);
    buf.extend_from_slice(R::NS_PREFIX);
    buf
}

/// Primary key entry (slot 0): `[header][slot 0][key payload]` — the
/// same assembly as `Collection::primary_key` (document.rs), no engine.
pub fn primary_key<R: okm_core::Document>(key: &R::Key) -> Vec<u8> {
    let mut buf = primary_header::<R>();
    buf.extend_from_slice(&okm_core::PRIMARY_SLOT.to_be_bytes());
    buf.extend_from_slice(&key.encode());
    buf
}

// ================= store assembly =================

/// One fjall `Database`, every collection handle cloned from it. `put`
/// takes `&mut Collection`; mudrad is the single writer.
#[cfg(feature = "engine")]
pub struct MudraStore {
    pub tags: Collection<FjallStore, TagKey, Tag>,
    pub pages: Collection<FjallStore, PageKey, Page>,
    pub page_tags: Junction<FjallStore, PageTag>,
    pub instances: Collection<FjallStore, InstanceKey, Instance>,
    pub site_widths: Collection<FjallStore, SiteWidthKey, SiteWidth>,
    pub state: Collection<FjallStore, StateKey, State>,
    pub history: Collection<FjallStore, HistoryKey, History>,
    db: FjallStore,
}

#[cfg(feature = "engine")]
impl MudraStore {
    pub fn open(path: &Path) -> fjall::Result<Self> {
        let store = FjallStore::open(path, "mudra")?;
        Ok(Self {
            tags: Collection::new(store.clone()),
            pages: Collection::new(store.clone()),
            page_tags: Junction::new(store.clone()),
            instances: Collection::new(store.clone()),
            site_widths: Collection::new(store.clone()),
            state: Collection::new(store.clone()),
            history: Collection::new(store.clone()),
            db: store,
        })
    }

    /// Persist the fjall database (Buffer mode: flush at a durable point).
    pub fn persist(&self) -> fjall::Result<()> {
        self.db.persist()
    }

    /// The shared engine handle (FjallStore clones are Arc-handle views
    /// of the same keyspace). The daemon's panel WS receiver builds a
    /// bare `NestStorage` over this — frames execute byte-identical in
    /// the collections' keyspace (SCHEMA's wire-protocol section).
    pub fn engine(&self) -> FjallStore {
        self.db.clone()
    }
}

// ================= counters & epoch =================

#[cfg(feature = "engine")]
impl MudraStore {
    fn state_u64(&self, slot: u8) -> u64 {
        self.state
            .get(&StateKey { slot })
            .map(|s| {
                let mut b = [0u8; 8];
                b.copy_from_slice(&s.value.0[..8]);
                u64::from_be_bytes(b)
            })
            .unwrap_or(0)
    }

    fn put_state_u64(&mut self, slot: u8, v: u64) {
        self.state.put(
            &StateKey { slot },
            &State { value: Bytes(v.to_be_bytes().to_vec()) },
        );
    }

    /// Allocate the next id from a per-table counter row. Under the
    /// single-writer model this is contention-free — chosen over a
    /// `HighWater` reduce for its simplicity (SCHEMA decision).
    pub fn next_id(&mut self, slot: u8) -> u64 {
        let v = self.state_u64(slot) + 1;
        self.put_state_u64(slot, v);
        v
    }

    /// Invalidations seen so far.
    pub fn epoch(&self) -> u64 {
        self.state_u64(state::EPOCH)
    }

    /// Bump after every CDP-driven write; return the new epoch. The panel
    /// re-scans on a changed number; the value never rides payloads.
    pub fn bump_epoch(&mut self) -> u64 {
        let v = self.state_u64(state::EPOCH) + 1;
        self.put_state_u64(state::EPOCH, v);
        v
    }

    /// Read a text setting slot; empty when unset.
    pub fn state_text(&self, slot: u8) -> String {
        self.state
            .get(&StateKey { slot })
            .map(|s| String::from_utf8_lossy(&s.value.0).into_owned())
            .unwrap_or_default()
    }

    pub fn put_state_text(&mut self, slot: u8, text: &str) {
        self.state.put(
            &StateKey { slot },
            &State { value: Bytes(text.as_bytes().to_vec()) },
        );
    }
}

// ================= typed query surface =================

// Probe encodings mirror the write side: integer fields go big-endian
// (`fields(...)` encoding = the raw BE bytes), `func`-index String
// probes are the function result's naked bytes (one gram per prefix
// scan, like `okm-core/tests/index_test.rs` drives `ByLower`). Recall is
// the index's job; precision filters (deleted/hidden flags) run on the
// fetched rows — cheap over hundreds of tags / thousands of pages.
#[cfg(feature = "engine")]
impl MudraStore {
    /// Children of a tag node, rank-ordered (root: `parent_id == -1`).
    /// Soft-deleted nodes drop out here — the tree column never shows them.
    pub fn tag_children(&self, parent_id: i32) -> Vec<(TagKey, Tag)> {
        let mut rows: Vec<(TagKey, Tag)> = self
            .tags
            .scan::<ByTagParent>(&parent_id.to_be_bytes())
            .into_iter()
            .filter_map(|(pk, row)| row.filter(|t| t.deleted == 0).map(|t| (pk.decoded, t)))
            .collect();
        rows.sort_by_key(|(_, t)| t.rank);
        rows
    }

    /// Rows addressed by bare tag name (the `/tags` API semantics).
    pub fn tag_by_name(&self, name: &str) -> Vec<(TagKey, Tag)> {
        self.tags
            .scan::<ByTagName>(name.as_bytes())
            .into_iter()
            .filter_map(|(pk, row)| row.filter(|t| t.deleted == 0).map(|t| (pk.decoded, t)))
            .collect()
    }

    /// Pages of one instance. Live by default; `include_deleted` widens
    /// to the closed-and-soft-deleted history.
    pub fn pages_of_instance(&self, instance_id: u32, include_deleted: bool) -> Vec<(PageKey, Page)> {
        self.pages
            .scan::<ByInstance>(&instance_id.to_be_bytes())
            .into_iter()
            .filter_map(|(pk, row)| {
                row.filter(|p| include_deleted || p.deleted_at == 0)
                    .map(|p| (pk.decoded, p))
            })
            .collect()
    }

    /// Subtree of pages opened by `parent` (CDP openerId sorting).
    pub fn page_children(&self, parent_id: u64) -> Vec<(PageKey, Page)> {
        self.pages
            .scan::<ByPageParent>(&parent_id.to_be_bytes())
            .into_iter()
            .filter_map(|(pk, row)| row.filter(|p| p.deleted_at == 0).map(|p| (pk.decoded, p)))
            .collect()
    }

    /// The live row whose CDP target id is exactly `target_id`. Consumers
    /// are CDP-driven (`/open {tabId}`, focus): a target that exists is
    /// an open page, so closed rows do not match. A rebind of target_id
    /// moves the index entry (see lifecycle `upsert_target`).
    pub fn page_by_target(&self, target_id: &str) -> Option<(PageKey, Page)> {
        self.pages
            .scan::<ByTarget>(target_id.as_bytes())
            .into_iter()
            .filter_map(|(pk, row)| {
                row.filter(|p| p.closed_at == 0 && p.deleted_at == 0)
                    .map(|p| (pk.decoded, p))
            })
            .max_by_key(|(_, p)| p.opened_at)
    }

    /// n-gram URL recall. A page appears once per matching gram, so hits
    /// dedup by id; the caller may rerank with `okm_ngram::bm25_score`.
    pub fn url_search(&self, query: &str) -> Vec<(PageKey, Page)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for gram in okm_ngram::ngrams(query, 3) {
            for (pk, row) in self.pages.scan::<ByUrl>(gram.as_bytes()) {
                if let Some(p) = row.filter(|p| p.deleted_at == 0)
                    && seen.insert(pk.decoded.id)
                {
                    out.push((pk.decoded, p));
                }
            }
        }
        out
    }

    /// Instance rows for a situation leaf name.
    pub fn instance_by_profile(&self, profile: &str) -> Vec<(InstanceKey, Instance)> {
        self.instances
            .scan::<ByProfile>(profile.as_bytes())
            .into_iter()
            .filter_map(|(pk, row)| row.map(|r| (pk.decoded, r)))
            .collect()
    }

    /// Every remembered width, site-ordered (port of `col show`'s SQL
    /// ORDER BY site; the scan yields id order, so re-sort on the name).
    pub fn site_widths_all(&self) -> Vec<SiteWidth> {
        let mut rows: Vec<SiteWidth> = self
            .site_widths
            .scan_keys()
            .into_iter()
            .filter_map(|k| self.site_widths.get(&k))
            .collect();
        rows.sort_by(|a, b| a.site.cmp(&b.site));
        rows
    }

    /// Remember (or replace) a site's proportion; returns the bumped
    /// epoch when the stored value actually changed (epoch discipline:
    /// writing the same proportion again is a silent no-op).
    pub fn site_width_put(&mut self, site: &str, proportion: f64) -> Option<u64> {
        let q = okm_core::Quant::<4>::new(proportion);
        if let Some((k, mut row)) = self.site_width(site) {
            if row.proportion == q {
                return None;
            }
            row.proportion = q;
            self.site_widths.put(&k, &row);
        } else {
            let k = SiteWidthKey { id: self.next_id(state::SITE_WIDTH_ID) as u32 };
            self.site_widths.put(&k, &SiteWidth { site: site.to_string(), proportion: q });
        }
        Some(self.bump_epoch())
    }

    /// The SiteWidth row for a host, if remembered.
    pub fn site_width(&self, site: &str) -> Option<(SiteWidthKey, SiteWidth)> {
        self.site_widths
            .scan::<BySite>(site.as_bytes())
            .into_iter()
            .find_map(|(pk, row)| row.map(|r| (pk.decoded, r)))
    }

    /// One open-verb visit: bump the History row for `url` (create on
    /// first sight). Visits always count — no epoch-bump discipline here:
    /// History is not a panel-visible collection (nothing re-scans on it),
    /// and the open write's own notification already rides the verb.
    pub fn history_bump(&mut self, url: &str, title: &str, ts: u64) {
        let hit = self
            .history
            .scan::<ByHistoryUrl>(url.as_bytes())
            .into_iter()
            .find_map(|(pk, row)| row.map(|r| (pk.decoded, r)));
        match hit {
            Some((k, mut row)) => {
                row.visits += 1;
                row.last_at = ts;
                if !title.is_empty() {
                    row.title = title.to_string();
                }
                self.history.put(&k, &row);
            }
            None => {
                let k = HistoryKey { id: self.next_id(state::HISTORY_ID) };
                self.history.put(&k, &History {
                    url: url.to_string(),
                    title: title.to_string(),
                    visits: 1,
                    last_at: ts,
                });
            }
        }
    }

    /// Every history row (id order; the caller ranks).
    pub fn history_all(&self) -> Vec<History> {
        self.history
            .scan_keys()
            .into_iter()
            .filter_map(|k| self.history.get(&k))
            .collect()
    }

    /// Last-known title for a URL from any live page row (the open verb
    /// uses it to refresh the history label). Full-table scan: acceptable
    /// at page scale, and this runs inside the verb's short lock anyway.
    pub fn page_title_for_url(&self, url: &str) -> Option<String> {
        self.pages
            .scan_keys()
            .into_iter()
            .filter_map(|k| self.pages.get(&k))
            .filter(|p| p.url == url && p.deleted_at == 0 && !p.title.is_empty())
            .max_by_key(|p| p.opened_at)
            .map(|p| p.title)
    }

    /// Tag ids linked to a page (junction A side: page → tags).
    pub fn tags_of_page(&self, page: &PageKey) -> Vec<TagKey> {
        page.get_tag(&self.page_tags)
    }

    /// Pages carrying a tag (junction B side: tag → pages).
    pub fn pages_of_tag(&self, tag: &TagKey) -> Vec<PageKey> {
        tag.get_page(&self.page_tags)
            .into_iter()
            .map(|pk| pk.decoded)
            .collect()
    }

    /// Link a page to a tag (idempotent: junction entries are keyed).
    pub fn link_page_tag(&mut self, page: &PageKey, tag: &TagKey) {
        self.page_tags.link(page, tag);
    }

    pub fn unlink_page_tag(&mut self, page: &PageKey, tag: &TagKey) {
        self.page_tags.unlink(page, tag);
    }
}
