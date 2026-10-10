# ADR: Extension protocol — mudra hosts resident subprocesses over stdio (BGI wire contract)

> **Languages:** [English](ADR-extension-protocol.md) (primary) · [中文](ADR-extension-protocol.zh-CN.md)

Date: 2026-10-01 (v2, same day)  Status: **Accepted (design finalized;
implementation not started)**

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

Why BGI and not LSP: LSP-style capability negotiation solves the
coordination problem of **two independently-evolving peers** — no editor or
language server can require the other to implement the whole spec, so the
capability list is the only way to find the intersection. A mudra extension
is one-directional: the host provides capabilities, the extension decides
what to use; the wire vocabulary is static and known by both sides (same
machine, same-version discipline, decode failure *is* the version-mismatch
signal — it does not need a negotiation to pre-empt it). This is exactly the
shape aura already named and shipped — **BGI** (Booth Gateway Interface), a
FastCGI-lineage resident bridge with typed host frames, per-target static
reply semantics (`Tier::Hot/Cold`), and one per-language wrapper as the
portability surface. mudra adopts the contract; it does not invent a second
wire.

## Decision

### 1. An extension is a resident subprocess (bgi shape)

Declared in `config.kdl` (`[extensions]` group: name → executable path —
routing metadata only: which processes to run, nothing about what they
listen to). mudrad spawns and supervises: `PDEATHSIG`, zombie-probing
`/proc` state, restart with backoff — the existing spawn discipline reused
wholesale. An extension never binds a port; its only socket is the pipe.

### 2. Wire: BGI frames, json-lines codec only

The frame vocabulary is aura ADR-0035 §3 (`call` / `result` / `host` /
`host_reply`, plus the `iterate` arms — see Consequences for why they ship
unexercised). Codec: **json-lines only** — CBOR is aura's declared upgrade
for carriers that bring a codec; a local single-host surface does not need
it. Bad discriminators fail at decode, not mid-flight.

### 3. Handshake: hello carries the interface_schema

```
mudra → { "type": "initialize", "protocol": 1, … }
ext   → { "type": "hello", "name": "adfree", "schema": { "on": ["mudra:page_open", …] } }
```

Event subscriptions are declared in the **script's interface_schema**
(aura's established declaration surface — ADR-0016 `@cron`, ADR-0026 §4
decorator-derived schema), **not in config**: the author knows what the code
listens to, and re-editing a deployment config to change a subscription is
friction config should never carry. An out-of-process binary cannot be
introspected by the host the way an uploaded script can, so the BGI shim
carries the schema out as session data — the registration moment, expressed
on the pipe. The daemon fans out only declared kinds — no broadcast, no
hidden dispatch. Names are free literal strings; the schema is a static
declaration, not a negotiation.

### 4. Delivery = BGI `Tier`, not a new vocabulary

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

### 5. Effects execute in mudra, never in the extension

The extension process is the **decision plane** (LLM calls, rules, state
all live there). Effects ride `host: {type: "invoke", …}` frames back into
mudra's verb surface — the invoke arm maps onto the existing verb directory
(the 8899 surface); injection scripts are governed by the Hook collection
and execute at the existing `INJECT_JS` point. No extension talks CDP
directly, no extension opens a second browser.

**The `store` arm is not implemented.** BGI's store plane is host-carried
booth state (aura ADR-0026: each booth type occupies a real okm ns in the
realm's engine). Extension state lives with the extension (its own
directory, or k10r over k10r's HTTP) — every datum has exactly one home.
The event log is *host*-owned and is delivery infrastructure, not extension
storage; do not conflate the two.

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

One per-language BGI shim runs in both worlds: an extension written against
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
  dispatch today. The shim answers them as unknown-kind errors if reached;
  a mudra-side iterate lands only when a consumer names itself.
- Withdrawn for good: prism transport; effector-style background→browser
  RPC (replaced by persistent intents — declared handlers + hooks);
  per-extension network services; wasm guests in page worlds (CSP,
  structural); mudra spawning or knowing k10r; namespace routing or
  auto-prefix mechanisms; LSP-style capability negotiation as a *model*
  (the reply contract is static + declared, not aligned between peers);
  v1's self-drafted fire/intercept frame kinds and `latency` class
  (subsumed by `Tier`).
