# ADR: mudra joins the aura federation — event outflow first, verbs as effectors

> **Languages:** [English](ADR-aura-probe-federation.md) (primary) · [中文](ADR-aura-probe-federation.zh-CN.md)

Date: 2026-09-29  Status: **Superseded (2026-10-01) by
[ADR-extension-protocol.md](ADR-extension-protocol.md)** — v3 was never
accepted; the prism transport and the effector-invoke leg are both withdrawn
(mudra↔k10r is direct RPC over k10r's own HTTP; browser effects are host-managed
extensions + hooks). Kept as the scenario record it was.

> v3 replaces the bespoke HTTP-endpoint transport with the **prism
> connection plane**: events are ordinary `{"ev": "mudra:<kind>"}` frames,
> the payload is pure data, and namespacing is a free convention (full
> literal strings, string-equality subscription) — no namespace routing
> rule in prism, no auto-prefix mechanism anywhere.

> v1 is superseded before ever being accepted: it led with an aura-side
> adapter booth and made verbs the primary surface, which relocates the
> operational entry point to prism and makes mudra DEPENDENT on the
> federation. The user's product ruling (2026-09-29): the user operates in
> mudra; mudra PUSHES page events out; aura-side booths (gravity, with its
> own krystallizer) consume them; verbs exist as effectors handlers may
  pull, not as the main door. Federation is for sharing, not for
  operating. v2 encoded that re-ordering; v3 now settles the transport
  onto prism.

## Context

mudra's control surface is unchanged from `ADR-rust-fullstack`: one daemon,
one store (fjall, single writer), 8899 verbs, the epoch bus for the panel.
Every lifecycle write already knows what it changed — the store's epoch
discipline returns `Some(new_epoch)` for real writes, `None` for no-ops —
and the daemon already fans a number out to the panel. The panel reacts by
re-pulling shaped reads.

That fan-out is the seam the federation plugs into. An external consumer
(a booth on an aura node, e.g. gravity) wants the same knowledge — what
page opened, what got tagged, what was navigated — without polling mudra
and without mudra depending on it being up.

The division of roles the product has fixed (PLAN §11 scenario rulings,
user 2026-09-29):

- **mudra is where the user operates** — panel, extension keys, CLI. It
  works fully alone; integration is opt-in configuration.
- **events flow OUT**: on lifecycle facts, mudrad emits a self-contained
  page event onto the configured connection plane (§1). "对方挂了捕获照常"
  — capture is mudra's own store; the outflow is a view of it, never its
  source.
- **handling lives on the aura side**: a booth consumes events (gravity's
  core is a loop — one event = one iteration, a natural fit); memory
  written in handling lands in THAT node's krystallizer. No memory data
  ever flows into mudra's store; no mudra business logic in the booth.
- **verbs are the effector surface**: when handling needs the browser to
  DO something (open a related page, tag it, close it), the booth calls
  mudra verbs. One-shot thin forwarding — not the main door.
- **federation (ADR-0031 explicit addressing) is for sharing**: e.g.
  "share this page to my team" forwards the event (addressed, cross-node)
  to a remote booth on the team's node. The share ACTION still happens in
  mudra.

## Decision

### 1. Page-event outflow — the primary integration surface

- **Trigger: the epoch bump, structured.** Every lifecycle write that
  bumps the epoch also builds an event: `page_opened` (insert/revive),
  `page_closed`, `page_deleted`, `tags_set`, `tag_created`,
  `ctx_switched`, `page_updated` (title/URL nav). No-ops emit nothing —
  the same branch the epoch discipline already draws.
- **Shape: self-describing, no fetch-back required.** An event carries
  what a handler needs to act WITHOUT mudra being online:
  `{kind, page_id, ctx, url, title, tag_ids, epoch, ts}` — a point-in-time
  snapshot, not a pointer. `page_id` remains the opaque cross-node
  reference; it is a correlation key, not a dereferenceable handle.
- **Delivery: fire-and-forget outside the store lock.** The daemon's
  short-lock discipline already forbids holding the store across an await;
  emission rides the same post-write, lock-free path as the epoch frame.
  A slow or dead endpoint must never stall a write — best-effort send,
  bounded timeout, drop on failure. **Backfill is a pull**: a consumer
  that missed events re-reads `/ctx_pages` + `/forest` (the store is the
  source of truth; events are the fan-out view). No sender-side queue:
  durability belongs to whoever needs it, and only the consumer knows
  what it needs.
- **Configuration: `config.kdl` `[events] prism = "ws://host/ws"` (absent
  = off).** Off by default — the zero-dependency product form is the
  default, integration is a line of config. Local panel fan-out (epoch
  numbers over the WS bus) is untouched; this is a second, structured,
  outward surface. The outbound WS client is not a new dependency
  (mudrad already speaks WS to CDP); one connection, drop-frames-while-
  disconnected, backoff-reconnect — the panel bus's own shape pointed
  outward.
- **Transport: prism event frames (prism ADR-0017).** Outbound =
  `{"ev": "mudra:page_opened", "args": {…}}` on the standard handshake
  (`?protocol=json` debug codec at first; CBOR is the receiver-side
  promotion when the plane needs it). Prism's reply frames
  (`<kind>.result` / `{"ev":"error"}`) are IGNORED by mudra — fire-and-
  forget stands with a protocol that answers: reading replies would make
  the outflow an RPC dependency; not reading them keeps a full prism
  offline behaviorally identical to the absent-config default. What the
  receiver plane does with the event (route to gravity, persist, record
  undeliverables) is the AURA side's decision — this ADR deliberately
  does not spec it.
- **Naming is a free convention, not a mechanism.** The emit site writes
  the FULL literal string (`mudra:page_opened`); subscribers match by
  string equality — exactly how dotted/colon names work on the plane
  today. Explicitly rejected: namespace parsing in dispatch (a routing
  rule for an aesthetic), an `@ns` decorator or an interface_schema
  field that auto-prepends (hidden machinery — the prefix vanishes from
  the author's view, users forget the wire name is not what they typed).
  The isolation boundary that DOES exist and is not for this purpose:
  realms are hard partitions; sharing over namespaces is deliberately
  convention-within-one-plane, so cross-app data (gravity on
  `mudra:*`) stays possible. Single source for the kind list: this ADR's
  trigger table + docs; consumers copy names from there, not from
  mudra's code.
- **Args are pure data.** The snapshot is readable JSON facts — never an
  Accrete envelope, never okm wire bytes, never a rendered form. The
  event's whole contract: self-contained data under a conventional name.

### 2. Handling lives on the aura side; memory stays home

- A booth (gravity) receives events and runs its loop per event; whatever
  it learns goes into its own krystallizer. mudra's store gains nothing
  from this ADR except — see §4 — a hook collection at implementation
  time. The probe iron rule (probe holds no storage, okm ADR-0010 §7) is
  honored by construction: nothing is held anywhere it shouldn't be.
- The booth's outbound capabilities toward the browser are exactly the
  verbs, called through a **thin forwarding handler** (v1's adapter,
  demoted to its right size): a three-line `invoke("open", args) → POST
  8899/open → return the JSON value` per verb; no verb logic, no caching,
  no store. Failure is the returned `{ok:false,err}` value (single
  envelope, ADR-0036 lineage), not a second channel.
- The effector surface is OPTIONAL: handlers that only observe and record
  memory never touch it.

### 3. Federation = sharing, with mudra as the action point

- "发现一个网页，分享到团队" is: a mudra-side action (verb `/share`, panel
  affordance) → event/memento forwarded to a remote booth addressed per
  ADR-0031 on the team's node → that node's gravity ingests it into the
  team's memory. Explicit addressing is the feature (remote-booths rule);
  the share never becomes "operate mudra from the remote node".
- v1's reversal is the anti-pattern named for the record: making the aura
  registry the place where mudra is operated splits the product into two
  entry points with a dependency direction (mudra → prism) that neither
  the daemon's design nor the user's workflow ever asked for. The v3
  transport keeps the direction honest: mudra speaks ON the prism plane
  as a producer — a down prism is a dropped frame, never a mudra failure.

### 4. Hooks (unchanged from v1; orthogonal to this correction)

- Page hooks (Tampermonkey-class) are **JS script data in a new store
  collection** (`Hook { id, name, match, script: Bytes, enabled: u8,
  added_at }`), governed by verbs (`/hook_add|remove|list|enable`),
  injected at navigation through the EXISTING CDP bridge (the INJECT_JS
  pipe; daemon-side URL-match selection). wasm guests are structurally
  excluded from page worlds by the same CSP facts as capsules
  (`wasm-unsafe-eval` absent on strict sites); JS is the only payload
  that runs everywhere the extension already runs.
- A hook's write-back rides the 8899 verbs like any local process — and
  a hook that reports facts worth remembering naturally feeds the SAME
  event outflow of §1 (hook → verb → lifecycle write → epoch bump →
  event): no second pipeline.

## Consequences

- mudrad gains: the event schema + emission path (lock-free post-write),
  the `[events]` config group, `/share` and hook verbs (implementation
  time, SCHEMA.md bilingual sync applies to the `Hook` collection).
- mudra loses nothing: with `[events]` absent, every byte of current
  behavior stands; no dependency edge mudra → aura/probe is added.
- aura side (this ADR records the landing points; each repo decides its
  own ADR): gravity grows an event-driven entry (one event = one loop
  iteration) and the thin verb-forwarding handler; undeliverable
  bookkeeping, if wanted, belongs to the receiving node's surfaces (the
  dead-ring precedent).
- Delivery semantics are deliberately weak (best-effort + pull backfill).
  If a future consumer proves it needs guaranteed event delivery, the
  answer is the consumer's store (apply the events into its own KV), not
  a sender-side queue in mudrad — mudra stays a browser, not a broker.

## References

- mudra: `PLAN.md` §11 open item (the scenario rulings this ADR re-orders
  faithfully), `docs/ADR-rust-fullstack.md` (single control point),
  `docs/WS-SYNC.md` (epoch bus), `docs/SCHEMA.md` (epoch discipline,
  Hook collection to come).
- aura: `docs/adr/0013` (federation over metadata consensus — data stays
  home: the memory-ownership symmetry of §2), `0031` (remote booths,
  explicit addressing), `0032` (booth), `0035` (exec carrier), `0036`
  (one envelope — failure as value).
- okm: `docs/adr/0010` §7 (probe holds no storage), `0027`/`0028`
  (transport-free intake; the browser storage family).
- prism: `docs/adr/0017` (the connection plane — frame shape, identity,
  broadcast traversal this ADR rides as a producer).
