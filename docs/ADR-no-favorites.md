# ADR: no favorites list — the rank axes already carry it

> **Languages:** English (primary) · [中文](ADR-no-favorites.zh-CN.md)

Date: 2026-10-21  Status: **accepted** (decision; no code changes — the data
model it points at is already implemented)

**Supersedes**: the launcher-era draft item in `docs/EXTENSIONS.md` that listed
"星级 / 收藏" as a data capability mudra still had to build (`site_stars` or a
state bit). That draft is retired; this ADR closes the path it proposed.

## Problem

A browser needs some way to say "keep this page". The conventional answer is a
favorites / bookmarks list: a page-level membership flag with its own store,
verbs and UI. Does mudra need one?

## Decision

No. mudra has no favorites list, no `star` / `unstar` verbs, and no reserved
collection for one.

A favorites list is a single membership bit. What a person actually means when
they "save" a page is one axis, or a combination of axes, of a space mudra
already keeps as tag trees — **importance ★ / quality ♥ / urgency 🔥**, and
further axes for further dimensions. The bit is a projection of that space onto
one dimension, so **the granularity is too coarse**: five levels on each of
three axes is 125 states the axes distinguish and the bit calls "saved".

The loss is not cosmetic — it discards *why* the page was kept:

- importance ★5 — "read this again, whenever"
- quality ♥5 — "good reference, not time-critical"
- urgency 🔥4 — "act on this within the week"

All three wear the same row in a favorites list, and the list cannot answer
"what should I read now" (urgency) versus "what should I cite later" (quality)
without re-deciding the reason by hand at read time.

The mechanism already exists. `rank` is a field on **Tag**, a rank axis is a tag
tree whose children carry `rank` 1..5 (0 = not a rank child, and the panel's k=0
"clear this axis"), and a page's score *is* the tag edge it carries
(`docs/SCHEMA.md`: Tag ns 1, `page_tag` double-written junction). The store even
seeds a non-rank sibling dimension, the `state` pipeline (to-read / reading /
distilled / archived) — a bookmark would compete with its "to-read" as well. The
write surface is there too (`/set_tags`, `/tag_set`, the panel's pips through
`set_rank`). A favorites list would therefore be a second, coarser copy of data
already in the same store — the exact failure the tag forest replaced for
sessions.

## Consequences

- The read surface for "saved pages" is the tag→page direction of `page_tag`,
  not a favorites table: "pages at importance ≥ 4" is a query over existing
  edges. Callers that want such a picker — e.g. `:open`'s third candidate
  source, alongside live pages and open history — read that direction.
- A dimension that is genuinely not one of the three becomes a **new rank
  axis**, not a membership flag. One vocabulary, extended by adding trees.
- What a folder uniquely offers — a hand-ordered list — is a *view* over rank
  and recency, not stored state. No ordering field is added.
- The control surface does not grow: no `/star`, no schema change, and
  `/open` / `/pages` stay as they are.

## Why not

- **A dedicated collection (`site_stars`, a `pinned` bit, a `/star` verb)** —
  rejected by the granularity argument: it duplicates the axes at one bit of
  resolution, and it is the path the retired launcher draft proposed.
- **A plain (non-rank) tag `starred`** — cheaper than a collection and still a
  collapse: it stands beside the axes as a *second* notion of "important", and
  having no level it can say "saved" but never "saved how much, and why".
- **An app-side list kept only in the panel** — two sources of truth for one
  question; the panel would have to reconcile with the axes it already writes.