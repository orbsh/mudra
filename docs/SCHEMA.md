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
  - URL reverse lookup: n-gram index (`okm-ngram` recipe — multi-value
    func index, n-gram → entries, caller-side rerank). Replaces the
    earlier full-scan-plus-predicate plan.
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
  - `page_id` / `tag_id` / `instance_id` / `site_width_id` / `history_id`
    — per-table auto-increment counters (u64). okm ships no built-in
    sequence; under the single-writer model a State counter row has no
    contention, so counters win over a `HighWater` reduce.

### History (ns 7)

- key: `id: u64` (auto-increment via the `history_id` counter).
- payload: `url`, `title`, `visits` (u64), `last_at`.
- access method: `by_url` func index over the full url (variable length,
  never key materialized — the fixed-width discipline; the write path
  finds the row to bump by exact url without a full scan).
- semantics: one row per opened address. `/open` and `/add` bump visits +
  last_at; the title label is refreshed at open time from the watcher-
  synced page rows. Nothing outside the open verbs ever writes History —
  visits are theirs alone. Writing history does NOT bump the epoch:
  History is panel-invisible; the open verb's own notification already
  rides along.
- consumption: `POST /history {query, limit}` — address-bar completion.
  Ranking is a pure function in mudra-store: subsequence similarity
  (`sim_score`) x visits, fused via RRF (k=60); the count ranks directly
  (no log — rank position is already a monotone statistic).

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
  it. Live stdio fan-out joins this when the extension host lands
  (pull is complete on its own: at-least-once, crash = don't advance).

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
