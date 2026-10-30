# ADR: Extension protocol — mudra hosts extension subprocesses over stdio (the Induction wire contract)

> **Languages:** [English](ADR-extension-protocol.md) (primary) · [中文](ADR-extension-protocol.zh-CN.md)

Date: 2026-10-01 (v2, same day)  Status: **Accepted; event-log slice
implemented 2026-10-01** — ns 8 `Event` collection (SCHEMA bilingual),
lifecycle-orchestrated emission (page_open / page_close / tag_set),
`POST /events {cursor, limit}` read-only replay window. The extension host
(stdio spawn/handshake/fan-out) implemented 2026-10-10 (`243fca4` —
hand-written BGI-shape frames, live smoke passed both planes) and re-based on
the standalone `induction-protocol` / `induction-host` crates (2026-10-13:
the frames, the budgeted session I/O, the PDEATHSIG spawn AND the arm
dispatcher are the library's — mudra drives the shared `route_frame` core
effector's process carrier also runs; the event pull and the verb surface
stay mudra's). The intercept (`Hot`) face landed 2026-10-13 (§4 below):
`mudra:before_navigate` verdicts (allow / deny / redirect), host-assigned
call ids, a fail-open budget, gated at `/open` and `/nav goto`. Both the log
and the hook set got their console views the same day (`events` / `hooks`
tabs; the log reads `{tail: N}` and every response carries `head`).

**Update 2026-10-30 — residency, and an event that answers.** Two corrections to
what this document first decided. (a) Residency is a **budget** question, not a
launch setting: nothing is started at boot unless it declares `resident true`, a
delivery cold-starts the guest, and the session reaps itself `idle_ttl` seconds
after its last one (§1b). A guest that declares the Hot event is never reaped,
whatever its declaration says — the 200 ms verdict cannot pay for a cold start.
(b) An event can carry an answer: the row id IS the call id, so a trigger can
return a value (§4b, `mudra:fill_password` — the address bar's `b`).

**Supersedes**: [ADR-aura-probe-federation](ADR-aura-probe-federation.md)
(v3, proposal) — the prism transport leg and the effector-invoke leg are both
withdrawn. The k10r integration becomes a worked *example* of an extension;
mudra knows nothing of k10r.

**v2 (2026-10-01)** replaces the self-drafted frame vocabulary with the
**BGI wire contract** (aura ADR-0035 §3 + its reply-contract Update), and
retracts the LSP framing: extensions are the plugin lineage (host-managed
booths), not a bridge between peers that must align heterogeneous
implementations. v1's two delivery planes collapse onto BGI's existing
`Tier` semantics; v1's `latency` class is deleted as redundant.

## Context

The control surface of mudra is RPC (the `niri msg` philosophy): every
capability is externally visible and scriptable, so functionality grows
outside the compiler. One event — a page opening — may have **several**
consumers at once (categorize it, block its ads, restyle it). If each
consumer were a network service, the user would be running a service
registry: too much to manage.

k10r's move to an independent service (krystallizer ADR-0007 as revised)
changed the shape of this problem: mudra → k10r is a direct RPC edge
(queryable, replayable), not an event-bus federation. Prism adds a process
dependency and a frame translation for a link that is local and single
consumer; and push over any bus never carried a delivery guarantee anyway.
So the federation transport is dropped entirely, and what remains — many
local consumers on one event stream — is answered by hosting them, not by
messaging them.

Why Induction and not LSP: LSP-style capability negotiation solves the
coordination problem of **two independently-evolving peers** — no editor or
language server can require the other to implement the whole spec, so the
capability list is the only way to find the intersection. A mudra extension
is one-directional: the host provides capabilities, the extension decides
what to use; the wire vocabulary is static and known by both sides (same
machine, same-version discipline, decode failure *is* the version-mismatch
signal — it does not need a negotiation to pre-empt it). This is exactly the
shape aura already named and shipped — **BGI** (Booth Gateway Interface), a
FastCGI-lineage process bridge with typed host frames, per-target static
reply semantics (`Tier::Hot/Cold`), and one per-language wrapper as the
portability surface — since extracted into the standalone **Induction**
protocol crates (`~/world/induction`). mudra adopts the contract through
those crates; it does not invent a second wire.

## Decision

### 1. An extension is a subprocess on a pipe (the induction shape)

Declared in `config.kdl` (`extensions` group: name → executable path or
`{ cmd; idle_ttl; resident }` — routing metadata only: which processes to run,
nothing about what they listen to). `cmd` may be absolute, `~/…` (against HOME),
or relative, in which case it resolves against the directory of the config file
that declares it — the repo's own config names `scripts/guests/bw_fill.py` and
works wherever the repo is checked out. mudrad spawns and supervises:
`PDEATHSIG`, zombie-probing `/proc` state, restart with backoff — the existing
spawn discipline reused wholesale. An extension never binds a port; its only
socket is the pipe.

### 1b. Residency is decided by the delivery budget

The first version of this document said "resident subprocess" and meant it
literally: `supervise` read the config at boot and started every declaration,
which then stayed parked for the life of the daemon. That is a FastCGI reflex,
and it is wrong for a guest that is asked something once an hour — the induction
shape is *cold start on demand plus a retention window* (aura's
`lifecycle.idle_ttl` + evictor), and mudra now does the same:

- **Nothing starts at boot** unless the declaration says `resident true`.
- A delivery (an event row, a verdict call) starts the guest: `ensure` spawns,
  handshakes, registers — and only then does the caller write the row it wants
  answered, because a session snapshots its cursor at the handshake and would
  otherwise read that row as history.
- `last_activity` moves on every delivered frame; `idle_ttl` seconds later the
  session reaps itself, and the next delivery cold-starts it. Default 60 s,
  `0` means never.
- **Exception, derived and not configurable:** a session that declares a Hot
  event is never reaped. A 200 ms verdict cannot pay for a spawn + handshake, so
  a reaped Hot guest would leave the gate failing open — silently, forever. The
  budget decides residency; the declaration only says what the guest wants.

Measured cost of the cold start this buys back: `python3` interp 16 ms (median of
20), against one `rbw list` at 138 ms for the guest below — the spawn is noise
next to the work.

### 2. Wire: Induction frames, json-lines codec only

The frame vocabulary is `induction-protocol`'s (`call` / `result` / `host` /
`host_reply`, plus the `iterate` arms — see Consequences for why they ship
unexercised); the library records it as the extraction of aura ADR-0035 §3 —
aura is where the names came from, the library is their home. Codec:
**json-lines only** — CBOR is the upgrade the contract declares for carriers
that bring a codec; a local single-host surface does not need
it. Bad discriminators fail at decode, not mid-flight.

### 3. Handshake: hello carries the interface_schema

```
mudra → { "type": "initialize", "protocol": 1, … }
ext   → { "type": "hello", "name": "adfree", "schema": { "on": ["mudra:page_open", …] } }
```

Event subscriptions are declared in the **script's interface_schema** (the
declaration surface the contract inherits — the practice started in aura
ADR-0016 `@cron` / ADR-0026 §4), **not in config**: the author knows what the code
listens to, and re-editing a deployment config to change a subscription is
friction config should never carry. An out-of-process binary cannot be
introspected by the host the way an uploaded script can, so the Induction
guest carries the schema out as session data — the registration moment, expressed
on the pipe. The daemon fans out only declared kinds — no broadcast, no
hidden dispatch. Names are free literal strings; the schema is a static
declaration, not a negotiation.

### 4. Delivery = the declared `Tier`, not a new vocabulary

`Tier` is declared beside the handler in the script's schema — per-handler,
static, never guessed at runtime (ADR-0035 Update):

- **Intercept semantics = `Hot` + deadline.** Pre-navigation verdicts:
  `{call, event: "mudra:before_navigate", args}` must be answered with
  `{result}` within the deadline; expiry is a failure value (the
  outer-Result discipline — the caller never hangs). mudra's policy maps
  expiry → **fail-open** (navigation UX wins): blocking a page because a
  verdict was late is a worse default than letting it through.
- **Observe semantics = `Cold`.** Event fan-out never parks the producer:
  dispatch returns Pending; the handler may answer later via `host` frames
  or not at all. Events also land in the **append-only event log** (mudra
  store; ns assigned at implementation start): a crashed or restarted
  extension replays from its own cursor — the consumer owns the checkpoint
  (krystallizer ADR-0008 trailing discipline: append-only log = free
  re-run/lag).

A handler is one shape or the other; there is no frame kind that is
simultaneously fire-and-forget and request-response.

**Implemented (mudra's side, 2026-10-13).** The call rides
`mudra:before_navigate` with args `{url, ctx}`; the answer is
`{"verdict":"allow"|"deny"|"redirect", "url"?}`. mudra assigns the call id from
a space disjoint from the event-log row ids (a guest answering a delivered
event can never be mis-correlated), waits a host-side budget (200 ms — the
per-handler `Tier`/deadline slot in the hello schema is reserved but carried by
nobody yet), and takes the first non-`allow` verdict in registration order
(name order). A `deny` answers the caller
`{"blocked":true,"by":<extension>,"url":…}` and does not execute the verb; a
`redirect` rewrites the request's url. The gate sits on the two navigation
ENTRIES of 8899 — `/open` (link-click beacon, console, CLI) and `/nav` with
`cmd: goto` — and deliberately NOT on `run_verb`, so a guest's own
`host:invoke` cannot be gated by the verdict it is carrying out. Every failure
mode (no declaring session, expired budget, dropped channel, stalled queue,
unparsable answer) fails open.

### 4b. An event can answer (the ask face)

§4 says a handler is either Hot (a verdict) or Cold (fire-and-forget), with no
frame that is both. A third shape turned out to be needed, and it costs no new
vocabulary: **the row id is the call id.** A caller writes an event row, a guest
that declared that kind answers with `{id: <row id>, result}`, and mudrad hands
the value back to the caller that is waiting on that id.

- `POST /fill_password {url, ctx?, user_hint?}` → mudrad cold-starts the
  candidates, writes a `mudra:fill_password` row, waits `ASK_BUDGET` (15 s), and
  returns `{ok, event_id, answer}`. Not a Controller verb: it holds no store lock
  while it waits (that would park the single store mutex for 15 s), so it sits in
  the HTTP shell ahead of `run_verb`.
- The caller is usually the address bar (`b`, `:fill-password`), which then types
  the value into the focused password field with the prototype's own setter and
  an `input`/`change` pair — a plain `el.value =` is invisible to any framework
  with a value tracker.
- Failure is fail-CLOSED (unlike a verdict): no declaring guest, no answer within
  budget, or an ambiguous vault entry is an error string on the status bar. A
  late password is worth nothing, and a wrong one is worth less.
- The log keeps the request for free, so the console's `events` tab shows every
  fill attempt beside the lifecycle events. A standalone row moves no epoch (the
  log is panel-invisible), so this path bumps it explicitly — that bump is the
  wake signal the fan-out rides.

**Shipped guest:** `scripts/guests/bw_fill.py` — a hand-written Induction guest
(no client library) that answers from a Bitwarden vault through `rbw`. It matches
by **URI host**, never by entry name (`rbw get github` answers "multiple entries
found" in this vault: five of them), narrows by the username the page already
carries, and refuses rather than guessing when the entry is still ambiguous. A
locked vault is reported (`rbw` has nobody to prompt — stdin is the wire).

### 5. Effects execute in mudra, never in the extension

The extension process is the **decision plane** (LLM calls, rules, state
all live there). Effects ride `host: {type: "invoke", …}` frames back into
mudra's verb surface — the invoke arm maps onto the existing verb directory
(the 8899 surface); injection scripts are governed by the Hook collection
and execute at the existing `INJECT_JS` point. No extension talks CDP
directly, no extension opens a second browser.

**Implemented (mudra's side, 2026-10-13).** The Hook collection is ns 9
(`name` / `matches` / `code` / `enabled` / `updated_at`, upsert on the name,
no epoch bump) with `/hook_set`, `/hook_list`, `/hook_delete` behind it and
`mudra hook set|ls|rm` on the CLI, plus the console's `hooks` view (the same
three verbs, no second channel). Execution rides the same bridge: each
enabled hook ships as its OWN guarded document source (`matches` = a
comma-separated URL-substring list, empty = every page) so a syntax error
takes that hook down alone; the code is embedded literally — no `eval` /
`new Function`, which strict-site CSP forbids — and a write re-injects into
the live pages of running instances (a reload re-arms from the current set,
which is also how a disable or delete takes effect: running code cannot be
recalled).

**The `store` arm is not implemented.** That arm exists in the library for
aura's booth state (aura ADR-0026: each booth type occupies a real okm ns in the
realm's engine); mudra has no booths. Extension state lives with the extension
(its own directory, or k10r over k10r's HTTP) — every datum has exactly one
home. The event log is *host*-owned and is delivery infrastructure, not extension
storage; do not conflate the two.

**Arm-set note (the vocabulary lives in the library).** The arms a guest may
invoke are `induction-protocol`'s `Arm` — 11 of them: `invoke |
iterate.start/next/dispose | store | emit | timer.register | timer.cancel |
queue.depth | queue.skip_to_head | schema`. The library records that set as the
extraction of the aura ctx object's CALLABLE members (aura ADR-0043 §5 is where
the names came from, not a dependency edge of this repo). The arm strings are
the dotted method paths; `emit` carries NO reply (fire-and-forget by contract).
mudra ships `invoke` only and needs none of the storage/timer/queue arms (§5's
one-home ruling); when mudra's extension surface grows, adopt the library's
names verbatim rather than minting synonyms — one vocabulary across hosts.
Registration (`on`/`receives`) has no runtime arm: the child's load-time pass
assembles its registry and the hello/schema answer carries it (reserved
`__sleep`/`__wake` hooks ride the same array).

### 6. No extension-to-extension channel

Extensions share **only** the event log. If categorizer output should feed
the restyler, the categorizer writes it to its own home (extension state,
mudra store via the reverse plane, or k10r) and the restyler reads it
there. One shared substrate beats N point-to-point links — the cure for
"too many services to manage" is not a message bus between the services.

### 7. Trust model unchanged

Local machine = the trust boundary. The extension plane has no admission
control, same ruling as 8899. Cross-node identity, if ever needed, belongs
to a federation layer, not here.

### 8. Portability is the point

One per-language Induction guest runs in both worlds: an extension written against
this contract can be hosted by mudrad or by an aura effector unchanged
(k10r-sync is the first expected both-sides consumer). mudra depends on no
aura crate — the contract is the dependency (as `aura_alloc` is the ABI for
wasm guests).

## Worked example: the k10r integration

An extension declares `@on mudra:page_open / page_close / tag_set` (Cold),
extracts knowledge (preferences, topic classification), stores it in k10r
over k10r's own HTTP surface. mudra contains zero k10r-specific code; a
user without k10r simply does not run the extension.

## Consequences

- 8899 remains the human/CLI/panel surface; the extension plane is the
  fourth machine surface (stdio, host-managed). The two never cross:
  extensions talk to the daemon over the pipe, not to the HTTP port.
- Lightweight rule-based styling can stay pure Hook JS (data in the store,
  no process) — extensions are for logic that needs state, compute, or
  network; they do not replace the Hook mechanism.
- Event log = new collection (append-only, cursor-replayable); the ns slot
  and key layout get registered in SCHEMA at implementation start, per the
  schema-finalization discipline.
- The `iterate` arms ship unexercised: mudra has no streaming verb to
  dispatch today. The host's arm table declines them with an explicit error
  value if reached; a mudra-side iterate lands only when a consumer names
  itself.
- Withdrawn for good: prism transport; effector-style background→browser
  RPC (replaced by persistent intents — declared handlers + hooks);
  per-extension network services; wasm guests in page worlds (CSP,
  structural); mudra spawning or knowing k10r; namespace routing or
  auto-prefix mechanisms; LSP-style capability negotiation as a *model*
  (the reply contract is static + declared, not aligned between peers);
  v1's self-drafted fire/intercept frame kinds and `latency` class
  (subsumed by `Tier`).
