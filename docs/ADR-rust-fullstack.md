# ADR: Rust full-stack rewrite (mudrad + okm storage + leptos panel)

Date: 2026-09-26  Status: accepted, not yet implemented

## Context

mudra was built as Python (CLI + `mudrad` daemon) over sqlite, with a zero-build
solidjs/hyperscript panel and a zero-build MV3 JS extension (`frontend/`). Three
things changed the calculus:

1. The Python glue layer keeps producing a recurring bug class that the type
   system would prevent: zombie pids passing `os.kill(pid, 0)` liveness checks,
   module-level globals leaking across nested async scopes (`_LOOP`), silent
   `return` on missing deps (`websockets`), hand-framed WS bytes. These are
   recorded "blood-stained lessons" throughout the repo — they are symptoms of
   running the control plane in a dynamically typed glue language.
2. The tag-forest UI (tree drilling, capsules, command popup) is meant to be
   **extracted into a library** with consumers beyond mudra. A JS-only panel
   would force two implementations of that UI (JS + Rust), which is a
   single-source violation.
3. okm needs a second real consumer to pressure-test `VirtualStorage` /
   `NestStorage` / `okm-wire` (krystallizer is the first). mudra's access
   patterns are simple KV traversals — tag-path walks, prefix page scans,
   URL substring filters — which fit the engine contract ("search is the KV's
   own capability", no JOIN requirement).

The original justification for Python also needs restating honestly: part of it
was the **extension problem** — browser-side behavior is JS, and a dynamic
backend felt like cheap glue. That motivation is now taken over by the
aura+probe direction (see Mitigations): the glue plane becomes delivered
scripts/actor invocations, not the backend language.

## Decision

- **mudrad → Rust.** Single binary; tokio; CDP over WS; chromium spawn with
  `PDEATHSIG`. CLI, daemon, and control HTTP/WS collapse into one crate family.
- **Storage: sqlite → okm** (`VirtualStorage` trait, `fjall` engine). Schema
  changes stay "drop and recreate" (prototype policy unchanged).
- **Panel → leptos (wasm)**; the tag-forest UI is extracted as a standalone
  Rust crate consumed by the panel and by future non-browser hosts. The
  zero-build hyperscript arrangement is retired together with it.
- **Panel data plane = okm wire protocol.** The panel speaks `RemoteStore`
  (sender side of `VirtualStorage`) as `okm-wire` frames over the existing
  WebSocket; mudrad hosts the `NestStorage` receiver (`apply(bytes)`), exactly
  the probe `EmitStore` pattern. The bespoke WS op protocol
  (`pages_changed`/`forest`/`set_tags` RPCs) is replaced by the engine
  contract.
- **Server push = invalidation hints on the same WS.** mudrad pushes an epoch
  bump when CDP sync mutates the store; the panel re-scans. Data plane stays
  pure KV reads — no second semantic channel.
- **localStorage becomes an okm backend.** It is the only browser storage with
  a synchronous API, which matches the sync `VirtualStorage` trait directly
  (`get`/`set`/`removeItem` map to point ops; `scan_range` = full enumerate +
  sort, O(n), acceptable at mudra scale of thousands of pages). Landed in okm
  as a `localstorage` feature for the wasm target, not as mudra-specific glue.
  Used for the panel's own local UI state; business data still flows to
  mudrad's fjall.
- **mudra-keys stays MV3 JS, and its capsules are not a second
  implementation.** Compiling wasm inside a content script is governed by the
  *page* CSP — sites without `wasm-unsafe-eval` reject
  `WebAssembly.instantiate` (the extension's own pages allow it; the content
  script world does not inherit that). So shipping the panel's wasm bundle to
  the extension fails structurally on strict sites. Single-source form instead:
  the tag-forest crate exposes a render-to-string surface (no DOM dependency);
  mudrad calls it natively, ships capsule HTML in the state payload, and
  content.js drops it into the bar with click delegation back through the
  existing SW bridge. One crate, two render entry points (wasm component /
  SSR string). The command popup's keyboard interaction stays extension JS.

## Rationale

- Type safety on the control plane eliminates the recorded dynamic-language bug
  class at the structural level rather than by more tests.
- One component implementation (Rust tag-forest crate) instead of two — the
  decisive argument that overturns the earlier "keep hyperscript zero-build"
  position.
- The panel-as-`VirtualStorage`-client form removes the op protocol entirely:
  the same codec (`okm-wire`, whose reuse contract explicitly names "WebSocket
  message" as a valid transport) and the same receiver (`NestStorage`) serve
  probe, the panel, and mudrad internals — one protocol, not a bespoke second.
- Storage swap carries zero migration risk because the drop-and-recreate policy
  was already locked.

## Why Not

- **Keep Python, swap only storage.** Leaves the bug class (zombie pids, global
  leakage, silent degradation) on the critical control path and does not serve
  the stated motivation (eliminate the Python glue). Storage swap alone is the
  B-motive; this project chose the A-motive.
- **Keep zero-build hyperscript panel.** Once the tag-forest UI is extracted as
  a Rust library, hyperscript means maintaining a parallel JS tree UI — the
  build-chain cost of leptos is the cheaper of the two debts. (The 2026-09-05
  zero-build decision was correct under its own premise — no library extraction
  planned; the premise changed.)
- **IndexedDB as the browser-side okm backend.** Inherently async; the sync
  `VirtualStorage` trait cannot be implemented on it without inventing an
  async surface for a local cache that mudra does not need at >5MB scale.
  localStorage's synchronous API is the structural match.
- **wasm loaded directly by the content script.** The page CSP governs wasm
  compilation in the content-script world; strict sites (no
  `wasm-unsafe-eval`) reject it outright — this is a structural failure, not a
  cost judgment. The SSR-string route from the same crate serves the extension
  without a second implementation (Decision, last bullet).
- **CRDT/event-sourced panel↔daemon sync.** mudrad is the sole writer of the
  store (single control point invariant, locked 2026-09-05); the panel is a
  client. Multi-writer sync would dissolve that invariant for no use case.

## Mitigations

- **Blood-lesson port is an explicit deliverable of phase R1**: zombie-pid
  liveness (`/proc/<pid>/stat` state != `Z`), `SingletonLock` dead-pid handling,
  five extension cache points, WAYLAND/XDG env injection on spawn, `pkill`
  self-match, "never ready" diagnosis order — each ported with a test, not
  re-learned.
- **Migration = drop & recreate** the store (policy unchanged); no data-port
  code, no dual-write period.
- **Python tree stays until Rust E2E parity** (spawn→CDP sync→panel render),
  then is deleted in one commit — the old tree remains the behavioral spec in
  the meantime.
- **aura+probe direction (open, undetermined)**: mudrad's control verbs may be
  exposed as actor invocations, and delivered probe-carrier scripts may take
  over the extension-side glue plane. This supersedes the original "Python for
  glue" motivation. No carrier mechanism for in-browser JS is decided; marked
  open, not blocking R0–R4.
