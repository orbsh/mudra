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

use okm_core::{
    Bytes, Collection, DocumentEncode, FjallStore, Junction, JunctionEncode, KeyEncode, Quant,
    Ref,
};
use std::path::Path;

pub mod lifecycle;
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
}

/// One opaque raw value per State slot (text settings, u64 counters, the
/// epoch). The slot decides the interpretation; the codec stays generic.
#[derive(DocumentEncode, Clone, PartialEq, Eq, Debug, Default)]
#[ok_ref(StateKey)]
#[ok_ns(6)]
pub struct State {
    pub value: Bytes,
}

// ================= generated index markers =================

// `DocumentEncode` names each access-method marker
// `__OkmIndex_<Row>_<index>`. Aliases give scan call sites readable
// types; `Collection::scan::<I>` takes the marker as a type parameter.
use __OkmIndex_Instance_by_profile as ByProfile;
use __OkmIndex_Page_by_instance as ByInstance;
use __OkmIndex_Page_by_parent as ByPageParent;
use __OkmIndex_Page_by_target as ByTarget;
use __OkmIndex_Page_by_url as ByUrl;
use __OkmIndex_SiteWidth_by_site as BySite;
use __OkmIndex_Tag_by_name as ByTagName;
use __OkmIndex_Tag_by_parent as ByTagParent;

// ================= store assembly =================

/// One fjall `Database`, every collection handle cloned from it. `put`
/// takes `&mut Collection`; mudrad is the single writer.
pub struct MudraStore {
    pub tags: Collection<FjallStore, TagKey, Tag>,
    pub pages: Collection<FjallStore, PageKey, Page>,
    pub page_tags: Junction<FjallStore, PageTag>,
    pub instances: Collection<FjallStore, InstanceKey, Instance>,
    pub site_widths: Collection<FjallStore, SiteWidthKey, SiteWidth>,
    pub state: Collection<FjallStore, StateKey, State>,
    db: FjallStore,
}

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
            db: store,
        })
    }

    /// Persist the fjall database (Buffer mode: flush at a durable point).
    pub fn persist(&self) -> fjall::Result<()> {
        self.db.persist()
    }
}

// ================= counters & epoch =================

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

    /// The live row whose CDP target id is exactly `target_id`. A reopen
    /// re-targets the row (mudrad rebinds target_id on revival), so at
    /// most one row carries a given id among live pages; if history rows
    /// linger, the newest open wins.
    pub fn page_by_target(&self, target_id: &str) -> Option<(PageKey, Page)> {
        self.pages
            .scan::<ByTarget>(target_id.as_bytes())
            .into_iter()
            .filter_map(|(pk, row)| row.filter(|p| p.deleted_at == 0).map(|p| (pk.decoded, p)))
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

    /// The SiteWidth row for a host, if remembered.
    pub fn site_width(&self, site: &str) -> Option<(SiteWidthKey, SiteWidth)> {
        self.site_widths
            .scan::<BySite>(site.as_bytes())
            .into_iter()
            .find_map(|(pk, row)| row.map(|r| (pk.decoded, r)))
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
