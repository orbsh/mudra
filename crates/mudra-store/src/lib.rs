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

/// The store's error: okm's own refusal (ADR-0037/0038), re-exported so
/// the store's signatures name one type.
///
/// Absence and failure are different answers: every read verb here
/// answers `StoreResult<Option<_>>` / `StoreResult<Vec<_>>`, and every
/// write verb answers `StoreResult<()>` or `StoreResult<bool>`. mudrad's
/// verb layer answers `Result<_, String>`, so the daemon maps this into
/// its own error at the boundary (`.map_err(err_text)?`) — the typed
/// refusal stays visible all the way to the HTTP edge.
#[cfg(feature = "engine")]
pub use okm_core::Error as StoreError;

/// `Result` in the store's spelling — see [`StoreError`].
#[cfg(feature = "engine")]
pub type StoreResult<T> = std::result::Result<T, StoreError>;

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

/// Event identity: surrogate id, monotone — the replay cursor domain
/// (ADR-extension-protocol §4: consumers own their cursor, the log never
/// reorders what the single writer assigned).
#[derive(KeyEncode, Clone, PartialEq, Eq, Debug, Default)]
pub struct EventKey {
    pub id: u64,
}

/// Hook identity: surrogate id. `name` is the upsert identity (indexed),
/// never key materialized — the surrogate key discipline every table here
/// follows.
#[derive(KeyEncode, Clone, PartialEq, Eq, Debug, Default)]
pub struct HookKey {
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

// ============ addressing text (shared by Page and History) ============

/// The address as a human types it: the scheme and a leading `www.` come
/// off; host, path and query stay ("github.com/orbsh", never
/// "https://www.github.com/orbsh"). This is the url half of every row's
/// addressing text. The scheme is the one part of a URL that says nothing
/// about *which* page it is — it is the same on every row, so as text it
/// can only add noise — and it is not something anyone types in the bar.
pub fn typing_surface(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.strip_prefix("www.").unwrap_or(rest).to_lowercase()
}

/// One row's addressing text as the two halves the laws consume: the
/// typing surface and the lowercased title. Containment and BM25 treat
/// the pair as one text (the counts are all that matters); the
/// subsequence leg keeps them apart, because a title match must keep the
/// head-anchor bonus it would lose behind a long url prefix.
fn addressing_parts(url: &str, title: &str) -> (String, String) {
    (typing_surface(url), title.to_lowercase())
}

/// The query side of both ranked lookups: lowercased, whitespace-split.
/// A token is whatever was typed, one character included — that is what
/// keeps recall free of a length floor.
fn query_tokens(query: &str) -> Vec<String> {
    query.to_lowercase().split_whitespace().map(str::to_string).collect()
}

/// BM25 over the query's tokens, matched by substring containment in each
/// row's addressing text (`addr` + `title`). One home for the law both
/// ranked lookups call.
///
/// The statistics are EXACT, not an index's approximation of them: the
/// caller's scan IS the corpus census, so `N`, `df` (rows containing the
/// token) and `avgdl` (mean addressing-text length) come from the rows in
/// hand — no counter stand-in, no average over the recalled set. Recall
/// is exact and floor-free as well: a row qualifies iff it contains a
/// token, so a query of any length is answerable, and the ranking is the
/// usual BM25 shape — how rare the token is (idf), how often it occurs
/// (tf), how long the text is.
///
/// `rows` is `(id, addr, title, addressing length)` — the length rides in the
/// row because both callers already have it (the history corpus stores it, the
/// page path measures it once) and the law needs it twice; the result is
/// `(id, score)` ordered by score desc, id asc (an explicit tiebreak — a caller
/// walking the ranking must not have its first hit depend on scan order), and
/// rows containing no token are absent: a candidate no leg recalled
/// contributes nothing to the fusion.
fn token_bm25(rows: &[(u64, &str, &str, f64)], tokens: &[String]) -> Vec<(u64, f64)> {
    let n = rows.len() as f64;
    // each row carries its addressing-text length: the corpus stores it, and
    // the page path measures it once per call — measuring it twice inside the
    // score pass walked every character of the corpus twice per query
    let avgdl = if rows.is_empty() {
        1.0
    } else {
        rows.iter().map(|(_, _, _, dl)| *dl).sum::<f64>() / n
    };
    // one pass per token: tf is this row's occurrence count, df the corpus
    let mut by_token: Vec<(f64, Vec<f64>)> = Vec::with_capacity(tokens.len());
    for tok in tokens {
        let mut counts = Vec::with_capacity(rows.len());
        let mut df = 0.0;
        for (_, addr, title, _) in rows {
            let tf = (addr.matches(tok.as_str()).count() + title.matches(tok.as_str()).count()) as f64;
            if tf > 0.0 {
                df += 1.0;
            }
            counts.push(tf);
        }
        by_token.push((df, counts));
    }
    let mut scored: Vec<(u64, f64)> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, (id, _, _, dl))| {
            let mut score = 0.0;
            for (df, counts) in &by_token {
                if counts[i] > 0.0 {
                    score += okm_ngram::bm25_score(counts[i], *dl, avgdl, *df, n);
                }
            }
            (score > 0.0).then_some((*id, score))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0))
    });
    scored
}

// ================= Page (ns 2) =================

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
    pub const EVENT_ID: u8 = 13;
    pub const HOOK_ID: u8 = 14;
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
/// counts open-verb calls; `title` carries the last-known title for
/// candidate labels. One access method: `by_url` (keyed, variable length,
/// never key materialized) lets the write path find the row to bump by
/// exact url without a scan. The ranked read (`history_search`) has no
/// index on either half of its addressing text and cannot have one — see
/// [`HistoryIndex`], which is how that scan is paid for without touching
/// the engine per query.
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

// ================= History read model =================

/// One history row with its addressing text precomputed.
///
/// The precomputation is the point: `addressing_parts` folds and allocates,
/// and the bar asks once per typing pause — doing it per query meant two
/// `String`s per row per keystroke. `dl` is the BM25 length kept beside them
/// because the law reads that number twice (corpus average, then the row).
struct HistoryText {
    id: u64,
    url: String,
    title: String,
    /// `typing_surface(url)`: scheme and `www.` stripped, lowercased.
    addr: String,
    /// The lowercased title, kept apart from `addr` on purpose: the
    /// subsequence leg must not hand a title match the address's head anchor.
    title_lc: String,
    /// `addr` + `title_lc` character count — the BM25 document length.
    dl: f64,
    visits: u64,
    last_at: u64,
}

impl HistoryText {
    fn of(id: u64, h: &History) -> Self {
        let (addr, title_lc) = addressing_parts(&h.url, &h.title);
        let dl = (addr.chars().count() + title_lc.chars().count()) as f64;
        Self {
            id,
            url: h.url.clone(),
            title: h.title.clone(),
            addr,
            title_lc,
            dl,
            visits: h.visits,
            last_at: h.last_at,
        }
    }

    fn history(&self) -> History {
        History {
            url: self.url.clone(),
            title: self.title.clone(),
            visits: self.visits,
            last_at: self.last_at,
        }
    }
}

/// The address bar's corpus: every History row, resident, addressing text
/// already folded (see [`HistoryText`]).
///
/// Why resident: the ranked read is a full-corpus pass by construction — the
/// fuzzy leg is a subsequence test and no index answers "ghb" (SCHEMA.md,
/// History). Paying that pass with a table read cost one point read and one
/// payload decode per row on top of the per-row folding the law used to
/// repeat; on a 60k-row store that measured 490ms per query, and the bar
/// issues one per keystroke. Over resident rows the same law reads borrowed
/// text: no engine access, no per-row allocation.
///
/// Coherence is structural, not conventional: the daemon is the store's only
/// writer, so the two write paths (`history_bump`, `history_absorb`) update
/// the row here in the same call, and History has no delete path (ids are
/// never reused), so the corpus only grows. Built on the first read rather
/// than in `open`: a store that never shows the bar pays nothing.
#[derive(Default)]
struct HistoryIndex {
    rows: Vec<HistoryText>,
    /// id -> position in `rows`: the write path's O(1) update.
    at: std::collections::HashMap<u64, usize>,
}

impl HistoryIndex {
    fn upsert(&mut self, id: u64, h: &History) {
        match self.at.get(&id).copied() {
            Some(i) => self.rows[i] = HistoryText::of(id, h),
            None => {
                self.at.insert(id, self.rows.len());
                self.rows.push(HistoryText::of(id, h));
            }
        }
    }

    /// The empty query's answer: the most recently opened rows, newest first.
    ///
    /// No recall leg runs. The bar opens on an empty line, and that state asks
    /// "where was I", not "what matches" — so recency alone answers it, and it
    /// answers with a bounded heap: one pass, `limit` memory, no sort.
    fn recent(&self, limit: usize) -> Vec<History> {
        use std::cmp::Reverse;
        if limit == 0 {
            return Vec::new();
        }
        let mut top: std::collections::BinaryHeap<Reverse<(u64, u64, u64)>> =
            std::collections::BinaryHeap::with_capacity(limit + 1);
        for r in &self.rows {
            top.push(Reverse((r.last_at, r.visits, r.id)));
            if top.len() > limit {
                top.pop();
            }
        }
        // sorted ascending by `Reverse`, i.e. newest first
        top.into_sorted_vec()
            .into_iter()
            .map(|Reverse((_, _, id))| self.rows[self.at[&id]].history())
            .collect()
    }

    /// The four-leg ranking over the resident corpus — the law
    /// [`MudraStore::history_search`] documents.
    fn search(&self, q: &str, limit: usize) -> Vec<History> {
        let rows = &self.rows;
        let texts: Vec<(u64, &str, &str, f64)> = rows
            .iter()
            .map(|r| (r.id, r.addr.as_str(), r.title_lc.as_str(), r.dl))
            .collect();
        // ---- leg 1: the addressing text (tokens, BM25) ----
        let text_rank: std::collections::HashMap<u64, usize> =
            token_bm25(&texts, &query_tokens(q))
                .into_iter()
                .enumerate()
                .map(|(r, (id, _))| (id, r))
                .collect();
        // ---- leg 2: the fuzzy leg (subsequence over address or title) ----
        // the query is the only side collected: the halves are already folded
        let qc: Vec<char> = q.chars().collect();
        let mut fuzzy: Vec<(u64, f64)> = rows
            .iter()
            .filter_map(|r| {
                let s = sim_chars(&qc, &r.addr).max(sim_chars(&qc, &r.title_lc));
                (s > 0.0).then_some((r.id, s))
            })
            .collect();
        fuzzy.sort_by(|a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0))
        });
        let fuzzy_rank: std::collections::HashMap<u64, usize> =
            fuzzy.iter().enumerate().map(|(r, (id, _))| (*id, r)).collect();
        // the candidates are what the recall legs found — nothing else is
        // ranked (visits and recency order the recalled set, they do not put
        // rows into it)
        let cands: Vec<&HistoryText> = rows
            .iter()
            .filter(|r| text_rank.contains_key(&r.id) || fuzzy_rank.contains_key(&r.id))
            .collect();
        if cands.is_empty() {
            return Vec::new();
        }
        // ---- per-leg orders (rank 0 = best), stable tiebreaks ----
        let mut visits_order: Vec<usize> = (0..cands.len()).collect();
        visits_order.sort_by_cached_key(|&i| {
            (std::cmp::Reverse(cands[i].visits), std::cmp::Reverse(cands[i].last_at), cands[i].id)
        });
        let mut recency_order: Vec<usize> = (0..cands.len()).collect();
        recency_order.sort_by_cached_key(|&i| {
            (std::cmp::Reverse(cands[i].last_at), std::cmp::Reverse(cands[i].visits), cands[i].id)
        });
        let rank_of = |order: &[usize]| -> std::collections::HashMap<u64, usize> {
            order.iter().enumerate().map(|(r, &i)| (cands[i].id, r)).collect()
        };
        let (visits_rank, recency_rank) = (rank_of(&visits_order), rank_of(&recency_order));
        let mut scored: Vec<(f64, u64)> = cands
            .iter()
            .map(|r| {
                let legs = [
                    text_rank.get(&r.id).copied(),
                    fuzzy_rank.get(&r.id).copied(),
                    visits_rank.get(&r.id).copied(),
                    recency_rank.get(&r.id).copied(),
                ];
                (rrf_legs(&legs, RRF_K), r.id)
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1))
        });
        scored.truncate(limit);
        scored.iter().map(|(_, id)| self.rows[self.at[id]].history()).collect()
    }
}

// ================= Event (ns 8) =================

/// One extension-protocol event: the append-only log half of the
/// observe plane (ADR-extension-protocol §4). `args` is the
/// self-describing JSON snapshot — consumers never call back into mudra
/// to interpret an event. No indexes: the replay path is a primary-order
/// cursor scan and nothing else reads this collection. Writing does NOT
/// bump the epoch — the log is panel-invisible (same discipline as
/// History). Retention is deferred (compaction design awaits a measured
/// need); ids never reused, the log is monotone by construction.
#[derive(DocumentEncode, Clone, PartialEq, Eq, Debug, Default)]
#[ok_ref(EventKey)]
#[ok_ns(8)]
pub struct Event {
    pub kind: String,
    pub args: String,
    pub at: u64,
}

// ================= Hook (ns 9) =================

fn hook_name(h: &Hook) -> String {
    h.name.clone()
}

/// One page-side hook: JS as *data* — the light-weight rule path of
/// ADR-extension-protocol §5 (no process, no pipe). Governance is the
/// 8899 verb surface; execution is the existing `INJECT_JS` injection
/// bridge, which composes one guarded source per enabled hook. `name` is
/// both the human handle and the upsert identity (`hook_upsert` is
/// idempotent on it, the `create_tag` rule). `matches` is a comma-
/// separated URL-substring list — empty matches every page. `enabled` is
/// u8 (okm's derive has no bool). Writing does NOT bump the epoch
/// (panel-invisible, same discipline as History and Event); the effect
/// path is a best-effort re-inject into live pages, which is a runtime
/// concern, not a store one.
#[derive(DocumentEncode, Clone, PartialEq, Eq, Debug, Default)]
#[ok_ref(HookKey)]
#[ok_ns(9)]
#[ok_index(by_name { func(hook_name) })]
pub struct Hook {
    pub name: String,
    pub matches: String,
    pub code: String,
    pub enabled: u8,
    pub updated_at: u64,
}

/// Address-bar style similarity: subsequence match of `query` over
/// `target` (case-insensitive), rewarding contiguous runs and early
/// matches. Returns 0.0..=1.0; a query that is not a subsequence is 0.
/// Pure function — the ranking lives here so mudrad, tests, and any
/// future consumer share one definition.
///
/// This is the bar's fuzzy leg (`history_search`): it answers the query
/// whose letters are in the address but not contiguous — no index can
/// look that up (the letters are not a substring of anything), so this is
/// the one leg that has to be paid for with a scan.
pub fn sim_score(query: &str, target: &str) -> f64 {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let t: String = target.chars().flat_map(char::to_lowercase).collect();
    sim_chars(&q, &t)
}

/// The law behind [`sim_score`], over an already-folded target.
///
/// The ranked read needs it this way: the corpus halves are folded once
/// when the row enters the corpus (`addressing_parts`), so a query must
/// not fold them again — the per-row `Vec<char>` was the read's dominant
/// allocation (2 halves x 2 allocs x N rows per keystroke). Folding an
/// already-folded string is the identity, so the two entry points agree
/// on every input.
fn sim_chars(q: &[char], target: &str) -> f64 {
    if q.is_empty() {
        return 0.0;
    }
    if target.is_empty() {
        return 0.0;
    }
    // greedy scan with run/position accounting: find q[] in t[] left-to-
    // right; every matched char scores 1, an extension of the previous
    // contiguous run scores +1 extra, matching at the very start scores
    // +1 extra (prefix bias).
    let mut qi = 0usize;
    let mut score = 0.0f64;
    let mut prev_match: Option<usize> = None;
    for (ti, tc) in target.chars().enumerate() {
        if qi < q.len() && tc == q[qi] {
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

/// Reciprocal-rank fusion over whichever legs a candidate appears in
/// (`None` = that leg did not recall it, which contributes nothing — the
/// standard missing-list treatment). k=60 is the standard smoothing
/// constant.
pub fn rrf_legs(ranks: &[Option<usize>], k: f64) -> f64 {
    ranks.iter().flatten().map(|r| 1.0 / (k + *r as f64 + 1.0)).sum()
}

/// RRF smoothing constant (standard paper value; also the panel default).
pub const RRF_K: f64 = 60.0;

// The two ranked laws (the address bar's `history_search`, the page
// fragment's `pages_by_fragment`) live on the store because they read the
// corpus. What stays free are the pieces they fuse: the fusion primitive
// above, the text leg's `token_bm25`, and the fuzzy leg's `sim_score`.

// ================= generated index markers =================

// `DocumentEncode` names each access-method marker
// `__OkmIndex_<Row>_<index>`. Aliases give scan call sites readable
// types; `Collection::scan::<I>` takes the marker as a type parameter.
// `pub` so the wasm panel (frame-level scans) addresses the same markers.
pub use __OkmIndex_Instance_by_profile as ByProfile;
pub use __OkmIndex_Page_by_instance as ByInstance;
pub use __OkmIndex_Page_by_parent as ByPageParent;
pub use __OkmIndex_Page_by_target as ByTarget;
pub use __OkmIndex_History_by_url as ByHistoryUrl;
pub use __OkmIndex_SiteWidth_by_site as BySite;
pub use __OkmIndex_Tag_by_name as ByTagName;
pub use __OkmIndex_Tag_by_parent as ByTagParent;
pub use __OkmIndex_Hook_by_name as ByHookName;

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
    // The slot carries its own width (ADR-0030: one byte for a fixed face, two
    // for a counting family) — hand-rolling `to_be_bytes` drifts the moment the
    // slot is a fixed face, which the primary face is.
    okm_core::write_slot(&mut buf, okm_core::PRIMARY_SLOT);
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
    pub events: Collection<FjallStore, EventKey, Event>,
    pub hooks: Collection<FjallStore, HookKey, Hook>,
    db: FjallStore,
    /// The bar's resident corpus, built on first read and kept in step by the
    /// two history write paths (see [`HistoryIndex`]). Interior mutability
    /// because a read populates it: the store lives behind one mutex and has
    /// one writer, so no borrow is ever concurrent.
    history_index: std::cell::RefCell<HistoryIndex>,
    history_index_built: std::cell::Cell<bool>,
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
            events: Collection::new(store.clone()),
            hooks: Collection::new(store.clone()),
            db: store,
            history_index: std::cell::RefCell::new(HistoryIndex::default()),
            history_index_built: std::cell::Cell::new(false),
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
    fn state_u64(&self, slot: u8) -> StoreResult<u64> {
        // `0` is the absent sentinel for every counter/setting slot, so a
        // missing row is absence — a failed read is the `?`.
        let Some(s) = self.state.get(&StateKey { slot })? else {
            return Ok(0);
        };
        let mut b = [0u8; 8];
        b.copy_from_slice(&s.value.0[..8]);
        Ok(u64::from_be_bytes(b))
    }

    fn put_state_u64(&mut self, slot: u8, v: u64) -> StoreResult<()> {
        self.state.put(
            &StateKey { slot },
            &State { value: Bytes(v.to_be_bytes().to_vec()) },
        )
    }

    /// Allocate the next id from a per-table counter row. Under the
    /// single-writer model this is contention-free — chosen over a
    /// `HighWater` reduce for its simplicity (SCHEMA decision).
    pub fn next_id(&mut self, slot: u8) -> StoreResult<u64> {
        let v = self.state_u64(slot)? + 1;
        self.put_state_u64(slot, v)?;
        Ok(v)
    }

    /// Invalidations seen so far.
    pub fn epoch(&self) -> StoreResult<u64> {
        self.state_u64(state::EPOCH)
    }

    /// Bump after every CDP-driven write; return the new epoch. The panel
    /// re-scans on a changed number; the value never rides payloads.
    pub fn bump_epoch(&mut self) -> StoreResult<u64> {
        let v = self.state_u64(state::EPOCH)? + 1;
        self.put_state_u64(state::EPOCH, v)?;
        Ok(v)
    }

    /// Read a text setting slot; empty when unset.
    pub fn state_text(&self, slot: u8) -> StoreResult<String> {
        match self.state.get(&StateKey { slot })? {
            Some(s) => Ok(String::from_utf8_lossy(&s.value.0).into_owned()),
            None => Ok(String::new()),
        }
    }

    pub fn put_state_text(&mut self, slot: u8, text: &str) -> StoreResult<()> {
        self.state.put(
            &StateKey { slot },
            &State { value: Bytes(text.as_bytes().to_vec()) },
        )
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
    pub fn tag_children(&self, parent_id: i32) -> StoreResult<Vec<(TagKey, Tag)>> {
        let mut rows: Vec<(TagKey, Tag)> = self
            .tags
            .scan::<ByTagParent>(&parent_id.to_be_bytes())?
            .into_iter()
            .filter_map(|(pk, row)| row.filter(|t| t.deleted == 0).map(|t| (pk.decoded, t)))
            .collect();
        rows.sort_by_key(|(_, t)| t.rank);
        Ok(rows)
    }

    /// Rows addressed by bare tag name (the `/tags` API semantics).
    pub fn tag_by_name(&self, name: &str) -> StoreResult<Vec<(TagKey, Tag)>> {
        Ok(self
            .tags
            .scan::<ByTagName>(name.as_bytes())?
            .into_iter()
            .filter_map(|(pk, row)| row.filter(|t| t.deleted == 0).map(|t| (pk.decoded, t)))
            .collect())
    }

    /// Pages of one instance. Live by default; `include_deleted` widens
    /// to the closed-and-soft-deleted history.
    pub fn pages_of_instance(
        &self,
        instance_id: u32,
        include_deleted: bool,
    ) -> StoreResult<Vec<(PageKey, Page)>> {
        Ok(self
            .pages
            .scan::<ByInstance>(&instance_id.to_be_bytes())?
            .into_iter()
            .filter_map(|(pk, row)| {
                row.filter(|p| include_deleted || p.deleted_at == 0)
                    .map(|p| (pk.decoded, p))
            })
            .collect())
    }

    /// Subtree of pages opened by `parent` (CDP openerId sorting).
    pub fn page_children(&self, parent_id: u64) -> StoreResult<Vec<(PageKey, Page)>> {
        Ok(self
            .pages
            .scan::<ByPageParent>(&parent_id.to_be_bytes())?
            .into_iter()
            .filter_map(|(pk, row)| row.filter(|p| p.deleted_at == 0).map(|p| (pk.decoded, p)))
            .collect())
    }

    /// The live row whose CDP target id is exactly `target_id`. Consumers
    /// are CDP-driven (`/open {tabId}`, focus): a target that exists is
    /// an open page, so closed rows do not match. A rebind of target_id
    /// moves the index entry (see lifecycle `upsert_target`).
    pub fn page_by_target(&self, target_id: &str) -> StoreResult<Option<(PageKey, Page)>> {
        Ok(self
            .pages
            .scan::<ByTarget>(target_id.as_bytes())?
            .into_iter()
            .filter_map(|(pk, row)| {
                row.filter(|p| p.closed_at == 0 && p.deleted_at == 0)
                    .map(|p| (pk.decoded, p))
            })
            .max_by_key(|(_, p)| p.opened_at))
    }

    /// Ranked page recall by a fragment of the addressing text (url or
    /// title) — the rerank `url_search` used to leave to the caller, done
    /// here so the law has one home (the `token_bm25` it shares with the
    /// address bar).
    ///
    /// Recall is substring containment and has no floor: a fragment
    /// shorter than any gram window is still a token, and the page a
    /// caller is about to look for is not less real for having been typed
    /// with two letters. Soft-deleted rows are filtered and are not part
    /// of the corpus statistics; liveness and context filters belong to
    /// the caller, which applies them in ranked order.
    ///
    /// No `limit`, deliberately: the ranking must not be cut before the
    /// caller's filters run — the row a context is looking for can sit
    /// below any fixed depth — so the caller's own walk bounds the work.
    pub fn pages_by_fragment(&self, query: &str) -> StoreResult<Vec<(PageKey, Page)>> {
        let tokens = query_tokens(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let mut rows: Vec<(PageKey, Page)> = Vec::new();
        for k in self.pages.scan_keys()? {
            // a row that vanished mid-walk is absence, not a shorter corpus
            if let Some(p) = self.pages.get(&k)?
                && p.deleted_at == 0
            {
                rows.push((k, p));
            }
        }
        let texts: Vec<(u64, String, String)> = rows
            .iter()
            .map(|(k, p)| {
                let (addr, title) = addressing_parts(&p.url, &p.title);
                (k.id, addr, title)
            })
            .collect();
        let view: Vec<(u64, &str, &str, f64)> = texts
            .iter()
            .map(|(id, addr, title)| {
                (*id, addr.as_str(), title.as_str(), (addr.chars().count() + title.chars().count()) as f64)
            })
            .collect();
        let at: std::collections::HashMap<u64, usize> =
            rows.iter().enumerate().map(|(i, (k, _))| (k.id, i)).collect();
        Ok(token_bm25(&view, &tokens)
            .into_iter()
            .map(|(id, _)| rows[at[&id]].clone())
            .collect())
    }

    /// Instance rows for a situation leaf name.
    pub fn instance_by_profile(&self, profile: &str) -> StoreResult<Vec<(InstanceKey, Instance)>> {
        Ok(self
            .instances
            .scan::<ByProfile>(profile.as_bytes())?
            .into_iter()
            .filter_map(|(pk, row)| row.map(|r| (pk.decoded, r)))
            .collect())
    }

    /// Every remembered width, site-ordered (port of `col show`'s SQL
    /// ORDER BY site; the scan yields id order, so re-sort on the name).
    pub fn site_widths_all(&self) -> StoreResult<Vec<SiteWidth>> {
        let mut rows: Vec<SiteWidth> = Vec::new();
        for k in self.site_widths.scan_keys()? {
            if let Some(row) = self.site_widths.get(&k)? {
                rows.push(row);
            }
        }
        rows.sort_by(|a, b| a.site.cmp(&b.site));
        Ok(rows)
    }

    /// Remember (or replace) a site's proportion; returns the bumped
    /// epoch when the stored value actually changed (epoch discipline:
    /// writing the same proportion again is a silent no-op).
    pub fn site_width_put(&mut self, site: &str, proportion: f64) -> StoreResult<Option<u64>> {
        let q = okm_core::Quant::<4>::new(proportion);
        if let Some((k, mut row)) = self.site_width(site)? {
            if row.proportion == q {
                return Ok(None);
            }
            row.proportion = q;
            self.site_widths.put(&k, &row)?;
        } else {
            let k = SiteWidthKey { id: self.next_id(state::SITE_WIDTH_ID)? as u32 };
            self.site_widths.put(&k, &SiteWidth { site: site.to_string(), proportion: q })?;
        }
        Ok(Some(self.bump_epoch()?))
    }

    /// The SiteWidth row for a host, if remembered.
    pub fn site_width(&self, site: &str) -> StoreResult<Option<(SiteWidthKey, SiteWidth)>> {
        Ok(self
            .site_widths
            .scan::<BySite>(site.as_bytes())?
            .into_iter()
            .find_map(|(pk, row)| row.map(|r| (pk.decoded, r))))
    }

    /// One open-verb visit: bump the History row for `url` (create on
    /// first sight). Visits always count — no epoch-bump discipline here:
    /// History is not a panel-visible collection (nothing re-scans on it),
    /// and the open write's own notification already rides the verb.
    pub fn history_bump(&mut self, url: &str, title: &str, ts: u64) -> StoreResult<()> {
        let hit = self
            .history
            .scan::<ByHistoryUrl>(url.as_bytes())?
            .into_iter()
            // The index is a prefix scan, so a short url also matches every
            // longer url that starts with it (`/` vs `/a/b`). Taking the first
            // hit merged unrelated rows; the row must be the exact one.
            .find_map(|(pk, row)| row.filter(|r| r.url == url).map(|r| (pk.decoded, r)));
        match hit {
            Some((k, mut row)) => {
                row.visits += 1;
                row.last_at = ts;
                if !title.is_empty() {
                    row.title = title.to_string();
                }
                self.history.put(&k, &row)?;
                self.sync_history_index(k.id, &row);
            }
            None => {
                let k = HistoryKey { id: self.next_id(state::HISTORY_ID)? };
                let row = History {
                    url: url.to_string(),
                    title: title.to_string(),
                    visits: 1,
                    last_at: ts,
                };
                self.history.put(&k, &row)?;
                self.sync_history_index(k.id, &row);
            }
        }
        Ok(())
    }

    /// One imported history row: fold an interchange record into the table.
    ///
    /// `max` rather than `+=` or assignment, because an import file describes a
    /// world state, not a stream of events: re-running the same import must be a
    /// no-op, while a fresher file still wins. It also keeps a stale export from
    /// walking a live row's counter or timestamp backwards. Returns whether the
    /// row was created (as opposed to folded into an existing one).
    pub fn history_absorb(
        &mut self,
        url: &str,
        title: &str,
        visits: u64,
        last_at: u64,
    ) -> StoreResult<bool> {
        let hit = self
            .history
            .scan::<ByHistoryUrl>(url.as_bytes())?
            .into_iter()
            // Exact url only: the index entry is a prefix of longer urls too.
            .find_map(|(pk, row)| row.filter(|r| r.url == url).map(|r| (pk.decoded, r)));
        match hit {
            Some((k, mut row)) => {
                row.visits = row.visits.max(visits);
                row.last_at = row.last_at.max(last_at);
                if !title.is_empty() {
                    row.title = title.to_string();
                }
                self.history.put(&k, &row)?;
                self.sync_history_index(k.id, &row);
                Ok(false)
            }
            None => {
                let k = HistoryKey { id: self.next_id(state::HISTORY_ID)? };
                let row = History {
                    url: url.to_string(),
                    title: title.to_string(),
                    visits: visits.max(1),
                    last_at,
                };
                self.history.put(&k, &row)?;
                self.sync_history_index(k.id, &row);
                Ok(true)
            }
        }
    }

    /// Every history row (id order; the caller ranks).
    pub fn history_all(&self) -> StoreResult<Vec<History>> {
        let mut rows: Vec<History> = Vec::new();
        for k in self.history.scan_keys()? {
            if let Some(h) = self.history.get(&k)? {
                rows.push(h);
            }
        }
        Ok(rows)
    }

    /// The resident corpus, built from the table on first use.
    ///
    /// Built here rather than in `open` so a store that never serves the bar
    /// pays nothing. This is the table's only scan left: point reads +
    /// payload decodes, once, instead of once per keystroke.
    fn ensure_history_index(&self) -> StoreResult<()> {
        if self.history_index_built.get() {
            return Ok(());
        }
        let mut idx = HistoryIndex::default();
        for k in self.history.scan_keys()? {
            if let Some(h) = self.history.get(&k)? {
                idx.upsert(k.id, &h);
            }
        }
        *self.history_index.borrow_mut() = idx;
        self.history_index_built.set(true);
        Ok(())
    }

    /// Keep the corpus in step with a write. An index that was never built has
    /// nothing to keep: its first build reads the table, which already carries
    /// the row.
    fn sync_history_index(&mut self, id: u64, row: &History) {
        if self.history_index_built.get() {
            self.history_index.get_mut().upsert(id, row);
        }
    }

    /// The address bar's ranking over the History log. Four legs, fused
    /// with RRF (equal weight, k=60):
    ///   1. the addressing text's BM25 — the query's tokens contained in
    ///      the scheme-stripped address or the title, weighted by how rare
    ///      each token is in the log (idf is what keeps a URL's noise from
    ///      scoring: what every address shares weighs nothing);
    ///   2. subsequence similarity (`sim_score`) over the same two halves
    ///      — the query whose letters are in an address but not
    ///      contiguous ("ghb" → github);
    ///   3. how often the address was opened, and 4. how recently — the
    ///      two signals every browser's bar has always ranked on.
    ///
    /// A row absent from a leg collects nothing from it: that is what puts
    /// a row recalled by BOTH recall legs ahead of a row recalled by one,
    /// and what keeps a heavily visited but unmatched address out of a
    /// non-empty query's list.
    ///
    /// Recall is still a pass over every row — the fuzzy leg is what forces
    /// it, and a subsequence is not a substring of anything, so no index can
    /// answer "ghb" (a character-set index answers by recalling most of the
    /// log: the same pass with worse locality). What makes the pass affordable
    /// is where it runs: over the resident corpus ([`HistoryIndex`]), with the
    /// addressing text already folded — not over the table. That is also why
    /// the two read indexes this law once had (`by_addr`, `by_title`) stay
    /// gone: they had nothing left to prune once the pass became the recall,
    /// and a `by_last_at` index would only serve the empty query, which the
    /// corpus answers from memory.
    ///
    /// The empty query is not a search: it answers the most recently opened
    /// rows, newest first — the bar's first state asks where you were, not
    /// what matches.
    pub fn history_search(&self, query: &str, limit: usize) -> StoreResult<Vec<History>> {
        self.ensure_history_index()?;
        let q = query.trim().to_lowercase();
        let idx = self.history_index.borrow();
        Ok(if q.is_empty() {
            idx.recent(limit)
        } else {
            idx.search(&q, limit)
        })
    }

    /// Append one event to the log; returns the assigned id (the live
    /// fan-out rides it — a replayed consumer and a live consumer see
    /// the same id). Append-only: no update or delete path exists, and
    /// the epoch never moves for a log write (panel-invisible collection).
    pub fn emit_event(&mut self, kind: &str, args: String, at: u64) -> StoreResult<u64> {
        let id = self.next_id(state::EVENT_ID)?;
        self.events.put(
            &EventKey { id },
            &Event { kind: kind.to_string(), args, at },
        )?;
        Ok(id)
    }

    /// Replay the log after `cursor` (id-ordered, capped). A consumer
    /// owns its checkpoint: it advances the cursor only past the rows it
    /// has durably handled.
    pub fn events_since(&self, cursor: u64, limit: usize) -> StoreResult<Vec<(u64, Event)>> {
        let mut out = Vec::new();
        for k in self.events.scan_keys()?.into_iter().filter(|k| k.id > cursor).take(limit) {
            if let Some(e) = self.events.get(&k)? {
                out.push((k.id, e));
            }
        }
        Ok(out)
    }

    /// The log's current head id (the counter, no allocation) — an
    /// extension session starts its cursor here: a fresh subscriber does
    /// not replay history (the aura ADR-0014 subscribe rule; a consumer
    /// that wants the past advances its own cursor through `/events`).
    pub fn event_head(&self) -> StoreResult<u64> {
        self.state_u64(state::EVENT_ID)
    }

    // ---- hooks (ns 9): the page-side rule path --------------------------

    /// One hook by its upsert identity (`name`), through the by_name func
    /// index — the write path's lookup, so the probe encoding cannot drift
    /// from the index's.
    pub fn hook_by_name(&self, name: &str) -> StoreResult<Option<(HookKey, Hook)>> {
        Ok(self
            .hooks
            .scan::<ByHookName>(name.as_bytes())?
            .into_iter()
            .filter_map(|(pk, row)| row.map(|h| (pk.decoded, h)))
            .next())
    }

    /// Idempotent upsert on `name`: an existing hook is updated in place
    /// (same id, new payload), `updated_at` advances either way. Returns
    /// `(id, created)`. No epoch bump — hooks are panel-invisible
    /// (SCHEMA ns 9, the History/Event discipline).
    pub fn hook_upsert(
        &mut self,
        name: &str,
        matches: &str,
        code: &str,
        enabled: u8,
        at: u64,
    ) -> StoreResult<(u64, bool)> {
        let row = Hook {
            name: name.to_string(),
            matches: matches.to_string(),
            code: code.to_string(),
            enabled,
            updated_at: at,
        };
        match self.hook_by_name(name)? {
            Some((k, _)) => {
                self.hooks.put(&k, &row)?;
                Ok((k.id, false))
            }
            None => {
                let k = HookKey { id: self.next_id(state::HOOK_ID)? };
                self.hooks.put(&k, &row)?;
                Ok((k.id, true))
            }
        }
    }

    /// Every hook, id order (the operator-facing read).
    pub fn hooks_all(&self) -> StoreResult<Vec<(HookKey, Hook)>> {
        let mut rows: Vec<(HookKey, Hook)> = Vec::new();
        for k in self.hooks.scan_keys()? {
            if let Some(h) = self.hooks.get(&k)? {
                rows.push((k, h));
            }
        }
        rows.sort_by_key(|(k, _)| k.id);
        Ok(rows)
    }

    /// The enabled hooks, id order — exactly what the injection bridge
    /// composes into guarded sources.
    pub fn hooks_enabled(&self) -> StoreResult<Vec<Hook>> {
        Ok(self
            .hooks_all()?
            .into_iter()
            .filter(|(_, h)| h.enabled == 1)
            .map(|(_, h)| h)
            .collect())
    }

    /// Delete by name; `false` when there was nothing to delete. The row does
    /// not have to be in hand: `purge` derives its removal set from the STORED
    /// row, so the lookup is only the existence check and the name-to-key hop.
    pub fn hook_delete_by_name(&mut self, name: &str) -> StoreResult<bool> {
        match self.hook_by_name(name)? {
            Some((k, _)) => self.hooks.purge(&k),
            None => Ok(false),
        }
    }

    /// Last-known title for a URL from any live page row (the open verb
    /// uses it to refresh the history label). Full-table scan: acceptable
    /// at page scale, and this runs inside the verb's short lock anyway.
    pub fn page_title_for_url(&self, url: &str) -> StoreResult<Option<String>> {
        let mut best: Option<Page> = None;
        for k in self.pages.scan_keys()? {
            if let Some(p) = self.pages.get(&k)?
                && p.url == url
                && p.deleted_at == 0
                && !p.title.is_empty()
                && best.as_ref().is_none_or(|b| p.opened_at > b.opened_at)
            {
                best = Some(p);
            }
        }
        Ok(best.map(|p| p.title))
    }

    /// Tag ids linked to a page (junction A side: page → tags).
    pub fn tags_of_page(&self, page: &PageKey) -> StoreResult<Vec<TagKey>> {
        page.get_tag(&self.page_tags)
    }

    /// Pages carrying a tag (junction B side: tag → pages).
    pub fn pages_of_tag(&self, tag: &TagKey) -> StoreResult<Vec<PageKey>> {
        Ok(tag
            .get_page(&self.page_tags)?
            .into_iter()
            .map(|pk| pk.decoded)
            .collect())
    }

    /// Link a page to a tag (idempotent: junction entries are keyed).
    pub fn link_page_tag(&mut self, page: &PageKey, tag: &TagKey) -> StoreResult<()> {
        self.page_tags.link(page, tag)
    }

    pub fn unlink_page_tag(&mut self, page: &PageKey, tag: &TagKey) -> StoreResult<()> {
        self.page_tags.unlink(page, tag)
    }
}
