# mudra — a keyboard browser mode for information-flow refining

> **Languages**: English (primary) · [中文](README.zh-CN.md)
>
> For most people the browser is the *primary inflow of information*. `mudra` is not
> just a way to drive the browser from the keyboard — it turns the browser entry point
> into an **information-flow pipeline**: **capture → attribute → refine → consume**.
>
> **Design rationale lives elsewhere**: `https://github.com/orbsh/wiki/blob/main/keyboard-wm-browser-mode.md`
> (Chinese) carries the *why* — positioning, hard constraints, selection, layer
> boundaries, rejected paths. This README is the implementation surface: commands, storage
> layout, CDP mapping, protocol shapes, status ledger. Generic tag-forest model:
> `https://github.com/orbsh/wiki/blob/main/tag-forest.md`.

Drives **`chromium --app`** windows externally over **CDP**, backed by an embedded
**okm/fjall KV store**, under a tiling WM (`niri`), fully keyboard-driven. Pages are
organized by a **tag forest** (multi-axis, replaces session) and scored by
importance / urgency, so the browser pre-sorts the inflow instead of just showing pages.

**Status**: Rust end to end — `mudra` (CLI) / `mudrad` (daemon) + okm/fjall store +
leptos wasm console; the Python tree is gone (2026-09-29). Core (spawn / realtime CDP
sync / navigation / niri window mapping+move / proxy+extensions / column-width memory)
works. The **tag forest** and the **wasm console** (`mudra ui`) are the management
surface; every lifecycle op goes through the `mudrad` control API. The **extension
plane** is live: lifecycle lands in an append-only event log, resident subprocess
extensions are hosted over stdio on the Induction contract, a guest declaring
`mudra:before_navigate` gets a pre-navigation verdict (allow / deny / redirect) under a
fail-open budget, and page-side **hooks** (ns 9) carry JS-as-data. Verified/pending
ledger: *Status & known issues* below.

**Requires** a WM with fine-grained IPC *and* unbounded workspaces — niri is the only
one that satisfies both; the argument why is in the wiki document, not here.

---

## Quick start

```bash
mudrad run                        # daemon: control API + WS push + Target→store sync
mudra open https://example.com    # open a page in the current context (spawns its instance)
mudra add https://example.com     # another page in the same instance
mudra ctx work                    # switch context (situation leaf)
mudra ls                          # per-context page counts
mudra ui                          # the web console
```

Every verb is a thin HTTP client of `http://127.0.0.1:8899`; the daemon is the only
process that spawns/kills windows and writes the store.

## Commands

| command | what it does |
|---|---|
| `mudra open <url> [--ctx LEAF]` | open a page in a context (spawns its instance if not running) |
| `mudra add <url> [--ctx LEAF]` | add a page to the context's running instance |
| `mudra close [query] [--ctx LEAF]` | close the whole context instance, or one open page (url filter) |
| `mudra ctx [LEAF]` | show / switch the current context (situation leaf) |
| `mudra ls [LEAF] [-f FILTER]` | per-context counts / pages of one ctx (url·title filter) |
| `mudra targets <ctx>` | list live page targets (CDP) |
| `mudra focus <query> [--ctx LEAF]` | ranked recall over the addressing text, first live hit in that ctx |
| `mudra goto <url>` / `back` / `forward` / `reload` `[--ctx LEAF]` | navigation |
| `mudra page move-here\|swap\|close <url> [--ctx]` | page-mode actions on the selected page |
| `mudra move <ctx> <workspace>` | move that context's windows to a workspace (niri) |
| `mudra conf <ctx> [--proxy <p>] [--ext <csv>]` | per-context proxy / extension list (applied on next open/add) |
| `mudra col [remember\|show] [site]` | column-width memory (niri) |
| `mudra tag init\|add\|remove [tag_id] [page_id]` | tag-forest seed / page assignment (`page_id` omitted = focused page) |
| `mudra hook set\|ls\|rm <name> [--code\|--file] [--matches] [--enable\|--disable]` | page-side hooks (ns 9) |
| `mudra sort mru\|mtime\|rating` | panel sort preference (state slot) |
| `mudra dev [on\|off]` | extension dev mode (clears chromium extension caches on spawn) |
| `mudra ui` | open the web console (focus an existing window or spawn) |

`mudrad` is not a subcommand: `mudrad run` runs as a systemd user service.

## Data model — okm collections on fjall

One store at `~/.local/share/mudra/store` (in developMode `~/.local/share/mudra` is a
repo symlink; the wrapper resolves `target/{release,debug}`). Full layout contract and
acceptance criteria: **`docs/SCHEMA.md`** (Chinese twin: `docs/SCHEMA.zh-CN.md`).

| ns | collection | key → payload |
|---|---|---|
| 1 | Tag | `id: u32` → `parent_id` (i32, `-1` = root sentinel), `name`, `alias`, flags `isolated`/`required`/`hidden`/`deleted`, `rank`, `note`; access `by_parent`, `by_name` func index |
| 2 | Page | `id: u64` → `instance_id`, `target_id` (CDP string), `url`, `title`, `position`, `opened_at`, `closed_at`, `deleted_at`, `parent_id` (opener page); access `by_instance`, `by_parent`, `by_target`; the addressing text is *not* indexed |
| — | page_tag | junction, no ns of its own: one edge entry lives in Page ns 2, the other in Tag ns 1 |
| 3 | *retired* | permanent hole — no slot reuse |
| 4 | Instance | `id: u32` → `profile` (situation leaf), `port`, `pid`, `running`, `proxy`, `extensions`; access `by_profile` |
| 5 | SiteWidth | site → column-width proportion (0–1) |
| 6 | State | fixed slots: `current_context`, `sort`, `dev_mode`, and the `tag_id` / `page_id` / `instance_id` counters |
| 7 | History | address visits, `by_url` func index; written by `/open` and `/add` via `history_bump`; **no epoch bump** |
| 8 | Event | append-only log (`page_open` / `page_close` / `tag_set`); `POST /events {cursor,limit}` or `{tail:N}`; every response carries `head`; checkpoints owned by the consumer; no epoch bump |
| 9 | Hook | `name`, `matches` (comma-separated URL substrings, empty = every page), `code`, `enabled`, `updated_at`; idempotent upsert by name (omitted fields keep their value); no epoch bump |

- **Migration policy (prototype)**: schema change = delete the store and rebuild — stop
  the daemon, `rm -rf ~/.local/share/mudra/store`, start, `POST /tag_seed`. No migration code.
- **Slot discipline**: slots are declaration order (okm ADR-0005). Dropping an index
  leaves dead bytes nothing reads — an existing store keeps working with no rebuild — and
  that position may not be re-declared without keeping the old index as `deprecated`.
- **Single writer**: fjall's file lock prevents a second writer from opening the store, so
  no DB-layer uniqueness constraint is needed for races.
- **Tag seed** (`POST /tag_seed`, idempotent): `situation` (inbox/work/personal/privacy —
  four isolated leaves, inbox required), `importance`/`urgency`/`quality` (☆..☆☆☆☆☆ five
  levels each, `rank` orders the stars), `state` (to-read/reading/distilled/archived),
  `topic`.

## Control API & daemon

- `mudrad` holds one CDP WebSocket per running instance, subscribes to Target events,
  writes the store, forwards control commands, spawns and stops instances. Singleton (flock).
- systemd user service on `graphical-session.target`; the start script gates on the
  Wayland env.
- Spawned instances carry `PDEATHSIG`: a dead daemon does not leave orphan browsers.
- Panel data comes from shaped reads on the same API (`/forest`, `/ctx_pages`, `/events`,
  `/hook_list`); the WebSocket carries only epoch invalidation frames, never data.
- **Epoch discipline**: a write reports whether anything actually changed, and only then
  does the epoch bump. The panel never polls — it hears the epoch number and re-pulls.
- Spawn flags: `chromium --app=<url> --remote-debugging-port=<dyn>
  --user-data-dir=<profile> --no-first-run`, plus `--proxy-server=<url>`
  (`--proxy-bypass-list` optional) and `--load-extension=<dir1>,<dir2>` from the instance
  row. The default extension is the in-repo `frontend/`.

## Pages, windows & CDP

- **Targets**: `Target.createTarget` opens, `Page.close` closes, `Target.activateTarget`
  focuses; `Target.getTargets` lists every target (url/title/type), so exact-page control
  works on error pages too.
- **Navigation**: `Page.navigate`, `Page.reload`, `Page.getNavigationHistory`,
  `Page.navigateToHistoryEntry` — all behind the single `/nav` verb.
- **Realtime sync**: `targetCreated` / `infoChanged` → upsert page (url/title live);
  `targetDestroyed` → set `closed_at`. No polling.
- **New-window interception (cascade)**: in `--app`, `_blank` / `window.open` still open a
  chrome-default window, so `mudrad` injects a script (override `window.open`, capture
  `a[target=_blank]` → Image beacon → local `/open` → a new `--app`). It must be injected
  into **every** new page target (`Target.targetCreated` →
  `Page.addScriptToEvaluateOnNewDocument`); otherwise a window mudrad itself spawned
  carries no script and its own `_blank` clicks fall back to chrome default.
- **Window ↔ instance mapping**: recorded at spawn (address, pid, window id) — CDP target
  ownership aligned to niri window ids, never to window titles.
- **Close semantics**: mudra-initiated close deletes the page (no trace); a WM close or a
  crash keeps the row and only marks `closed_at`. All three close paths (`close_target` /
  `close_page` / `mark_down`) emit `page_close` — a dead instance produces no
  `targetDestroyed`, so teardown must emit it.
- **Event log rules**: baseline replay at daemon start is not a page event (a restart is
  not a page open); the refresh branch emits `page_open` only when the URL actually
  changed — a title-only change is metadata, not an event.

## Instances, isolation & workspaces

- One instance per isolated tag dimension (own profile / cookie jar / cache / process).
  Nothing runs by default; `mudra open` spawns. Several instances run concurrently.
- Each isolated tag binds a niri workspace (`web:<tag>`). `mudra open` = focus that
  workspace + spawn the instance + rebuild its pages.
- Windows land there by mapping, not by title: `niri msg action focus-workspace <name>` at
  open time plus the recorded window ids.
- Windows of different instances may share one workspace; `mudra move <ctx> <workspace>`
  walks that instance's windows and issues `niri msg action move-window-to-workspace`.
  Moving is the "separate" operation.
- **Resource model**: process-per-page, so N `--app` windows in one instance cost what N
  tabs cost; separate instances cost a browser process + cache each. Instance count =
  isolation dimensions, never page count.

## Column-width memory (Win+R)

- A niri column width *is* an output-width proportion; `preset-column-widths` bands are
  1/3, 1/2, 2/3, 1 (`switch-preset-column-width`, Mod+R).
- Read (verified 2026-08): `niri msg -j windows` has no top-level width; the focused
  window's nested `layout.tile_size[0]` is the column width — divide by
  `focused-output.logical.width`.
- Set (verified): `set-column-width <N%>` takes a percentage (or `-N` pixels); the
  fraction form `1/2` errors. Re-apply by snapping the stored proportion to the nearest
  band and emitting an integer percentage.
- Capture: Win+R reads the focused window's proportion plus the focused page's host, snaps
  to a band, stores it per site; restore happens on open for that host.

## Keyboard: one address line, two open verbs, one close

- **The address bar is the command line** (2026-10-22, three rounds). `o` / `O` merely type
  `open ` / `new ` into the same input line. Once a space appears, candidates switch to
  *live pages (console role only) + open history*; Tab moves the selection across
  candidates, a lone candidate completes into the line, Enter takes the highlighted one
  (no match = the typed text is used). **There is no second-level picker** — one Enter
  either way, and no dead keystroke after `:open ` (an empty line + Backspace leaves
  command mode).
- **Two verbs**: `open` navigates in place (`location.href`; hint `f` and the picker's
  Enter use it), `new` opens another `--app` page window (hint `F`, i.e. mudrad `/open`).
  The console never navigates in place — always `new`.
- **URL normalization lives in one place** (`spawn::normalize_url`): a missing scheme
  becomes `https://`; a single-label bare word gets `.com` (`bing` → `bing.com`), while
  `localhost`, hosts with a port, userinfo, or a dot are kept as-is. Both verbs go through
  it, so a bare word in the address line is never a dead `https://bing`.
- **`:q`** closes the page that spoke it (2026-10-22, no default binding): the service
  worker calls `chrome.tabs.remove(sender.tab.id)` directly and does **not** use mudrad's
  `/close_page`, which matches url/title substrings and can pick the wrong page. The
  console can `:q` too — it is just another tab. The daemon needs no cooperation: the
  watcher sees `Target.targetDestroyed` and marks the row `closed_at`.
- One visual language covers the command line, the candidate popup above it and the status
  bar (`bg` hue + `bar.opacity` + `statusBlur` + `fg`). Keymap and config keys:
  `docs/KEYS.md`.

## History recall

Addressing text = scheme-stripped lowercased address + lowercase title. Recall and
scoring (2026-10-14):

- **Recall**: token substring **containment** with *no length floor* (a two-letter
  fragment is a token like any other) ∪ **subsequence** (`ghb` → github; `ycombinat` →
  news.ycombinator.com, matched mid-address).
- **Score**: BM25 over the addressing text (URL noise absorbed by idf, not by excluding
  URLs from recall) + visit count + recency → four equal-weight RRF legs. The scan that
  recalls is also the census, so the BM25 statistics are exact.
- Addresses hit by neither recall leg never appear under a non-empty query, however often
  they were visited. The empty query is not a search: it answers the most recently opened
  rows, newest first (the bar opens on an empty line and asks "where was I").
- `by_addr` / `by_title` are retired: a subsequence has no index to answer from, so recall
  is one full pass, and the index lost its pruning target. The pass runs over a resident
  corpus (`HistoryIndex`, folded addressing text, kept in step by the two write paths), not
  over the table: a table read per query measured 490 ms on the 60k-row store, the corpus
  answers in 10-40 ms and the empty query in ~1 ms (`docs/SCHEMA.md`, History).
- The bar asks **once per typing pause** (120 ms debounce) and **30 candidates deep** — three
  times the display depth, since the list scrolls. One query per keystroke was what made
  typing feel slow: the requests queued on the daemon's single store lock, so a six-letter
  word meant six passes end to end.

## Extension plane

Declared in `config.kdl` `[extensions]` as **name → executable path** (routing metadata
only). Subscriptions are declared in the guest's `interface_schema` and carried by the
`hello` frame — never in config, never negotiated. Wire contract: Induction
(`~/world/induction` crates, pulled as a git dependency like okm); mudra pulls in **no
aura crate** — the contract is the dependency. The CDP socket is `tungstenite`; CDP is
hand-rolled JSON with id-routed waiters and self-removing timeouts (no JSON-RPC envelope).

- **Delivery = the declared `Tier`**: `Hot` + deadline = intercept semantics (verdict
  before navigation; a timeout is a failure value that the host maps to fail-open);
  `Cold` = observe semantics (dispatch returns Pending, does not block the producer; the
  handler may answer later, or never).
- **Implemented shape (intercept, 2026-10-13)**: frame `mudra:before_navigate`, args
  `{url, ctx}`; reply `{"verdict":"allow"|"deny"|"redirect","url"?}`. Call ids are
  host-assigned in a space disjoint from event-log row ids. The wait budget is a host
  constant of 200 ms (`Tier`/deadline are reserved slots in the hello schema; library and
  guests do not carry them yet). Extensions run in registration (name) order and the
  **first non-allow** wins: `deny` answers the caller with
  `{"blocked":true,"by":<ext>,"url":…}` and does not execute the verb; `redirect`
  rewrites the requested url and proceeds. The gate hangs on the two navigation **entry
  points** — `/open` and `/nav` `goto` — and deliberately **not** on `run_verb` (a guest's
  own `host:invoke` must not be blocked by the verdict it is executing). Every failure
  face (no declared session, budget expiry, broken channel, full queue, unparsable reply)
  allows the navigation.
- **Hooks (the no-process half, ns 9)**: page-side JS as data — `name` / `matches` /
  `code` / `enabled` / `updated_at`, idempotent upsert by name, no epoch bump. Governance:
  `/hook_set` `/hook_list` `/hook_delete`, `mudra hook`, and the console's `hooks` view.
  Execution lands on the existing `INJECT_JS` bridge: **one guarded source per hook** with
  a literal substring guard (a syntax error kills only that hook), `code` inlined verbatim
  (no `eval`, strict site CSPs). Writing a hook triggers a best-effort re-inject into live
  pages of running instances; disable/delete only affect future documents — injected code
  cannot be recalled, a reload re-arms.
- **Not implemented**: the `store` arm (extension state belongs to the extension; the
  host-owned event log is delivery infrastructure, not storage) and the `iterate` arms
  (empty; touching one returns an explicit error value until a consumer signs up).
- Design: `docs/ADR-extension-protocol.md` (bilingual). Superseded draft:
  `docs/ADR-aura-probe-federation.md`.

## Web console

`mudra ui` — leptos wasm (trunk-built) in a `chromium --app` window. Three surfaces
switched by header buttons: **pages** (tag-forest tree, ctx selector, filter chips),
**events** (read-only, newest first, kind chips, re-pulled by `/events {tail}`),
**hooks** (CRUD over the same verbs: toggle / delete with confirmation / create / edit,
self re-pulled — hook writes do not bump epoch). No second channel is opened.
Architecture: `docs/PANEL.md`.

## Build & environment

- `cargo build --release` produces `mudra` (CLI) and `mudrad` (daemon). Panel =
  trunk-built wasm in `crates/mudra-panel/dist` (`wasm-bindgen-cli` pinned to the
  workspace version).
- Launching windows needs the Wayland env:
  `export WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000`. The niri socket is
  auto-discovered (`NIRI_SOCKET` or `/run/user/<uid>/niri.wayland-*.sock`).
- Git-sourced dependencies: okm (schema derives + fjall engine), induction (extension
  frames + host half). Also `tokio`, `tungstenite`, `kdl` (config parsing); panel side
  `leptos` + `web-sys`.
- Clipboard goes through Wayland `wl-copy` (focused window URL over CDP → pipe).

## Status & known issues

**Verified**: `--app` has no omnibox/tab strip and renders as a bare page under niri · CDP
lists and attaches `--app` windows (they are plain page targets) · niri
`move-window-to-workspace` / `move-column-to-workspace` / `set-column-width` exist, and
column width reads via `layout.tile_size[0]` ÷ `logical.width` · `--app` +
`--load-extension` loads unpacked extensions (2026-08-28, verified with SurfingKeys, from
2026-09-05 with mudra-keys itself) · self-authored mudra-keys runs fully in `--app` (four
modes, hints, scroll, `:set`, status bar — 2026-09-05) · tag forest landed 2026-09-05,
History + completion 2026-10-01 · Rust full stack landed 2026-09-28–29
(`cargo test --workspace`: 138 tests + live smoke) · extension plane 2026-10-01/10-10
(event log + `/events` replay, stdio host, `host:invoke`) · intercept verdicts and Hook
collection + console `events`/`hooks` views 2026-10-13.

**Pending (⏳)**: CDP reload/navigate on `chrome-error` pages (mechanically should work) ·
"one instance's multi-window ≈ multi-tab" resource equivalence (not measured) · the
refinement pipeline (html→md + full-text search, naive-Bayes scoring, RSS capture, LLM
summarization).

**Known issue (deferred)**: passwords are not saved in `--app` (no Linux keyring) —
candidates are `gnome-keyring` or `--password-store=basic`.

**Stale singleton trap**: a leftover chromium from a previous schema still holds the
profile's `SingletonLock`; a new spawn hands its flags to the old process and exits — the
debug port never listens and the daemon marks the instance down. If instances go "never
ready" after a profile/schema change, kill the old `profiles/*` chromium processes first.

**Open**: error-page control routing (no extension injection there — CDP only; which
operations are needed is still undefined) · download/print (chrome UI gaps) · tag-forest
app-layer single-select enforcement · ML scoring (rules learned from manual scores, for
review — not a black box) · whether hook disable/delete ever needs a "re-arm matching
pages" verb.

## Roadmap

See `PLAN.md` — P4/P5 done (proxy+extensions, column memory), P6 done (new-window
interception), P7a done (`WmExt` interface + niri backend); P7b (launcher menus) was
dropped 2026-09 — the console is the whole management surface. Tag-forest architecture and
the Rust rewrite landed 2026-09; the extension plane landed 2026-10-13. Next: the
refinement phases (P8–P12).

## Where the design lives

| document | role |
|---|---|
| `https://github.com/orbsh/wiki/blob/main/keyboard-wm-browser-mode.md` (+ `-en.md`) | **why** — positioning, hard constraints, selection, layer boundaries, rejected paths, open design questions (Chinese primary, English twin) |
| `https://github.com/orbsh/wiki/blob/main/tag-forest.md` | the generic multi-axis content model |
| `README.zh-CN.md` | Chinese translation of this file |
| `docs/SCHEMA.md` (+ `.zh-CN.md`) | storage layout contract (the acceptance gate for store changes) |
| `docs/KEYS.md` | keymap and extension config |
| `docs/HISTORY-INTERCHANGE.md` | the history exchange format (`:export` / `:import`) — the contract exporters conform to |
| `docs/PANEL.md` | console architecture and the tag-forest abstraction roadmap |
| `docs/EXTENSIONS.md` | extension authoring |
| `docs/ADR-*.md` | decision records — `rust-fullstack`, `self-maintained-extension`, `no-favorites` (bilingual), `extension-protocol` (bilingual), `aura-probe-federation` (superseded, bilingual) |
| `docs/WS-SYNC.md`, `docs/BASELINE.md` | sync design and baseline notes |