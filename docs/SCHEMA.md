# mudrad storage schema (okm on fjall) — R1 acceptance spec

> 中文版：[SCHEMA.zh-CN.md](SCHEMA.zh-CN.md)

Rust rewrite of mudrad replaces the sqlite file with a single fjall store
accessed through okm typed derives (`#[derive(DocumentEncode)]` +
`Collection`). This document is the layout contract for PLAN §11 / R1;
code review against it is the acceptance gate.

Design premises (locked in `ADR-rust-fullstack.md`, not reopened here):

- Static typed derives only — no `okm-dynamic` (mudrad is a Rust process;
  dynamic is for aura host languages).
- One engine (`fjall`), one store handle.
- Schema change = delete the store and rebuild (prototype policy).
- Key discipline: fixed-width binary segments, no separators.

## Collections

Namespaces are assigned in declaration order.

### Tag (ns 1)

- key: `id: u32` BE (auto-increment via the `tag_id` counter below).
- payload: `parent_id: i32` (`-1` = root sentinel), `name`, `alias`,
  flags `isolated` / `required` / `hidden` / `deleted`, `rank`, `note`.
- access methods:
  - `by_parent { fields(parent_id) }` — tree drilling main query.
  - `by_name` func index on `name` — bare-name addressing (the `/tags`
    and `/tag` API semantics address nodes by bare name, not path).
- No covering (`includes`) anywhere in R1: at prototype scale (hundreds
  of tags, thousands of pages) fetch-back after a prefix-converged scan
  is the cheap operation; a covering value with variable-length fields
  would buy a hand-decoded byte segment for nothing.

### Page (ns 2)

- key: `id: u64` BE (auto-increment via the `page_id` counter).
- payload: `instance_id`, `target_id` (CDP string id), `url`, `title`,
  `position`, `opened_at`, `closed_at`, `deleted_at` (soft delete: only
  closed pages may be deleted), `parent_id` (opener page, CDP openerId).
- access methods:
  - `by_instance { fields(instance_id) }` — list all pages of a context.
  - `by_parent { fields(parent_id) }` — subtree sorting query.
  - `by_target` func index on `target_id` — reverse lookup for
    `/open {tabId}` and `focus_page`; the URL fallback stays a fallback.
  - addressing-text lookup (`pages_by_fragment`): the ranked law the caller
    used to own, one home now. The addressing text is the scheme-stripped
    lowercased address plus the lowercase title; recall is substring
    CONTAINMENT of the query's tokens with no length floor (a two-letter
    fragment is a token like any other), and the score is BM25 over the
    corpus's own statistics — exact, because the scan that does the recall
    is also the census. Consumer: `/focus {query}`, which ranks this
    context's open live pages instead of taking whichever live target
    happened to come first. No index is declared for this text (the
    `by_text` gram index is gone): recall is containment, so posting lists
    had nothing left to prune, and a gram window has a floor a fragment can
    be shorter than. Layout note: the dropped index's entries stay in their
    slot as dead bytes that nothing reads — an existing store keeps working
    with no rebuild. Slots ARE declaration order (okm ADR-0005), so nothing
    may be declared in this position again without keeping the old index as
    `#[ok_index(…, deprecated)]` (slot reserved, writes stopped,
    `prune_deprecated_slots()` reclaims it); dropping and re-seeding the
    store (the prototype policy) reclaims the space too, and can wait.
- (instance, target) uniqueness: the sqlite tree guarded it with a
  UNIQUE index against multi-daemon races. Rust mudrad needs no DB-layer
  constraint — fjall's file lock makes a second writer impossible to
  open the store at all, and `upsert_target`'s identity order never
  inserts a row for a target that already has one.

### page_tag

Junction Page↔Tag, double-written edge entries. Cross-tree multi-select =
multiple rows; within-tree single-select is an app-layer constraint
(same semantics as the sqlite version).

`JunctionEncode` declares no ns of its own — the entries live one in
Page's ns 2 and one in Tag's ns 1 (direction is carried by which ns the
entry lives in; the segment-0x3 discriminator with `#[ok_junction(1)]`
marks the junction). ns 3 is therefore retired; it stays a permanent
hole per the no-reuse rule (ADR-0002).

### Instance (ns 4)

- key: `id: u32` (auto-increment via the `instance_id` counter).
- payload: `profile` (situation leaf name), `port`, `pid`, `running`,
  `proxy`, `extensions`.
- access method: `by_profile` func index on `profile`.

### SiteWidth (ns 5)

- key: `id: u32` (auto-increment via the `site_width_id` counter).
- payload: `site`, `proportion`.
- access method: `by_site` func index on `site`. The variable-length
  `site` string is deliberately not a key segment (fixed-width
  discipline; hashing it into the key would make the key a query
  dimension instead of an identity — index is the correct landing).

### State (ns 6)

- key: fixed `u8` slot from a closed enum; value: raw payload for the slot.
  No indexes — a fixed small set.
- slots:
  - `current_context` / `walker_mode` / `op_mod` / `sort` / `dev_mode` —
    same keys as the sqlite `state` table.
  - `epoch: u64` — invalidation counter (see below). Not part of any
    page/tag payload; it is transport-layer state.
  - `page_id` / `tag_id` / `instance_id` / `site_width_id` / `history_id` /
    `event_id` / `hook_id` — per-table auto-increment counters (u64). okm
    ships no built-in sequence; under the single-writer model a State counter
    row has no contention, so counters win over a `HighWater` reduce.

### History (ns 7)

- key: `id: u64` (auto-increment via the `history_id` counter).
- payload: `url`, `title`, `visits` (u64), `last_at`.
- access method: `by_url` func index over the full url (variable length,
  never key materialized — the fixed-width discipline): the WRITE path
  finds the row to bump by exact url without a full scan. The ranked read
  declares no index and cannot have one — see below.
- **the index is a prefix scan, so the write path must compare url text**:
  a lookup for `https://x/` also returns every longer url that starts with
  it, in key order — taking the first hit folds unrelated addresses into one
  row (`/` swallowed `/a`, `/a` swallowed `/a/b`). Both writers
  (`history_bump`, `history_absorb`) filter on `row.url == url`. At import
  scale the unfiltered version merged 6,314 of 60,122 rows into their
  prefixes; `tests/history_test.rs` is the regression gate.
- bulk write: `history_absorb` folds an interchange record (see
  `docs/HISTORY-INTERCHANGE.md`) taking `max()` of counter and timestamp, so
  re-importing a file is idempotent and a stale file cannot walk a live row
  backwards. The live open verb still uses `history_bump` (increment).
- semantics: one row per opened address. `/open` and `/add` bump visits +
  last_at; the title label is refreshed at open time from the watcher-
  synced page rows. Nothing outside the open verbs ever writes History —
  visits are theirs alone. Writing history does NOT bump the epoch:
  History is panel-invisible; the open verb's own notification already
  rides along.
- consumption: `POST /history {query, limit}` → `MudraStore::history_search`,
  the address bar's four-leg ranking fused with RRF (equal weight, k=60):
  1. the addressing text's BM25 — the query's tokens contained in the
     scheme-stripped address or the title. The URL's noise is controlled
     HERE, in scoring, not by holding the URL out of recall: idf gives what
     every address shares (a scheme, a common host) almost no weight, and
     the scheme is not part of the addressing text at all;
  2. subsequence similarity (`sim_score`) over the same two halves — the
     query whose letters are in an address but not contiguous ("ghb" →
     github);
  3. visits and 4. recency. These two order the recalled set; they never
     put a row into it.
  Recall is a scan of the log, and that scan is also the census: N, df and
  avgdl are the corpus's own numbers, not a counter stand-in or an average
  over the recalled set. Leg 2 is what forces the scan — a subsequence is
  not a substring of anything, so no index can answer "ghb", and the one
  index that could (a character set) answers by recalling most of the log,
  which is the same scan with worse locality. The two read indexes this
  replaced are gone for that reason: `by_addr` (the head-anchored prefix
  leg — the least useful half of "the URL as text" once the whole address
  is text again) and `by_title` (the title-only lexical leg) had nothing
  left to prune once the scan became the recall. Layout note: the retired
  slots keep their entries as dead bytes that nothing reads, so an existing
  store keeps working with no rebuild; since slots are declaration order
  (okm ADR-0005), nothing may be declared in those positions again without
  keeping the retired indexes as `deprecated` placeholders (slot reserved,
  writes stopped, `prune_deprecated_slots()` reclaims the entries).
  If the log ever grows enough to hurt, an index comes back in front of the
  scan as a pruner — never as the definition of recall.
  A row absent from a leg collects nothing from it, so an address matching
  neither recall leg never appears for a non-empty query however often it
  was opened. The empty query keeps the old rule: every row, visits order.

### Event (ns 8)

- key: `id: u64` (auto-increment via the `event_id` counter).
- payload: `kind` (free literal string, `mudra:` prefix), `args` (the
  self-describing JSON snapshot), `at` (u64 wall-clock millis, caller-
  supplied like every lifecycle timestamp).
- access method: none — the replay path (`events_since`) is a primary-
  order cursor scan; nothing else reads this collection (SCHEMA's
  no-cover-index rule extended: no secondary access need at all yet).
- semantics: the append-only half of the extension-protocol observe
  plane (ADR-extension-protocol §4). Emission is orchestrated in the
  lifecycle transitions (single source of truth — watcher and verb paths
  both flow through it): `mudra:page_open` on Insert/Revive/URL-changed
  Refresh (the baseline replay on watcher reconnect is NOT an event —
  consumers catch up via /ctx_pages snapshots), `mudra:page_close` on
  the watcher's destroyed path, the `/close` verb, and the `mark_down`
  sweep (corpses never deliver destroyed events). `mudra:tag_set`
  carries the FULL post-change id set — reconstruction from one row,
  never a delta to sequence. Writing does NOT bump the epoch
  (panel-invisible, same discipline as History). Retention/compaction
  deferred until a measured need; ids never reused, monotone by
  construction.
- consumption: `POST /events {cursor, limit}` — the read-only replay
  window; the consumer owns its checkpoint, the daemon never advances
  it. `{tail: N}` anchors the SAME read at the end (the newest N rows,
  id ascending — `head - N` is the cursor, exact because ids are a
  dense counter), and every response carries `head`, the counter. The
  live stdio fan-out reads by cursor, the console's log view by tail.
  Pull is complete on its own: at-least-once, a consumer that dies
  simply does not advance and re-reads.

### Hook (ns 9) — registered at implementation start, per the schema-finalization rule

- key: `id: u64` (auto-increment via the `hook_id` counter).
- payload: `name` (free literal string — the human handle AND the upsert
  identity), `match` (comma-separated URL substrings; empty = every page),
  `code` (the JS source, injected verbatim), `enabled` (u8 0/1 — okm's
  derive has no bool), `updated_at` (u64 wall-clock millis, caller-supplied).
- access method: `by_name` func index on `name` — `hook_set` is an upsert
  keyed on it (the `create_tag` (parent,name) idempotency rule); reads are
  full scans (hooks are a handful, no other secondary need). ABSENT fields
  in `hook_set` keep their stored values (a code-only edit must not reset
  the scope — the `instance_set_config` rule); `code` is required on
  create and cannot be blanked (delete the hook instead).
- semantics: the **light-weight rule path** of ADR-extension-protocol §5 —
  page-side JS as *data*: no process, no pipe. Governance is the 8899 verb
  surface (`/hook_set`, `/hook_list`, `/hook_delete`); execution is the
  existing CDP injection bridge (the `INJECT_JS` pipe): the daemon injects one
  `Page.addScriptToEvaluateOnNewDocument` source per enabled hook, guarded by
  a literal substring test, and runs that same source once in already-loaded
  pages (`Runtime.evaluate`). One source per hook on purpose: a syntax error
  kills that hook alone instead of the page's whole injection. The code is
  embedded literally — no `eval` / `new Function`, which strict-site CSP
  forbids (the injected source itself is not subject to it). Writing a hook
  does NOT bump the epoch (panel-invisible, same discipline as History and
  Event); the effect path is a best-effort re-inject into the live pages of
  running instances, so a hook takes effect without a reload.
- boundary: hooks are the "no process" half. Logic that needs state, compute
  or network belongs in an extension (a hosted subprocess), which reaches the
  same injection point through `host:invoke` — the Hook collection is not an
  extension-storage substitute.

## Epoch invalidation signal

mudrad bumps the `epoch` row after every CDP-driven write (page open /
destroy / title update, tag mutation). The panel's data plane is pure
VirtualStorage frame request/response, so push exists only as a hint:

- mudrad sends one frame on the already-open panel WS carrying nothing
  but the new epoch number; the panel responds by re-scanning pages.
- Correctness never depends on the frame: a lost frame only delays the
  panel's next self-initiated scan. The signal is a latency optimization,
  not a reliability mechanism.
- The epoch lives in State (not process memory) so it never rewinds
  across mudrad restarts; the panel compares against the last epoch it
  saw to decide whether to re-scan.
- The epoch is never embedded in page/tag payloads — data model stays
  free of push mechanics.

## Wire protocol note

The panel speaks raw VirtualStorage frames over the same WebSocket
(`VirtualStorageAsync` → `NestStorage::apply(bytes)`); okm type-layer
objects (`Collection`) stay sync and live only inside mudrad. The panel
encodes/decodes with pure-byte codecs and treats mudrad's namespace ids
and key layouts as this document defines them.
