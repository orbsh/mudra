# mudra — a keyboard browser mode for information-flow refining

> For most people the browser is the *primary inflow of information*. `mudra` is not
> just a way to drive the browser from the keyboard — it turns the browser entry point
> into an **information-flow pipeline**: **capture → attribute → refine → consume**.
>
> Rooted in the browser-mode design at `~/.hermes/wiki/keyboard-wm-browser-mode.md`
> (Chinese; this README is the English/usability surface). Generic tag-forest model:
> `~/.hermes/wiki/tag-forest.md`.

Drives **`chromium --app`** windows externally over **CDP**, backed by **sqlite**,
under a tiling WM (`niri`), fully keyboard-driven. Pages are organized by a **tag
forest** (multi-dimensional, replace-of-session) and can be scored by importance /
urgency, so the browser becomes a tool that *pre-sorts your information* instead of
just showing webpages.

**Status**: core (spawn / realtime CDP sync / navigation / niri window mapping+move /
proxy+extensions / column-width memory) works as CLI+daemon `mudra` / `mudrad`.
**Context model** (tag forest, *replaces session* — pages hang directly off an
instance; the instance is chosen by the current `situation` leaf) is implemented: a
**solidjs web console** (`mudra ui`) is the management surface, all lifecycle ops go
through the **mudrad control API** (open/add/close/ctx switch + WebSocket push).
See `PLAN.md` §9, `docs/PANEL.md`, and below.

---

## Why another browser mode

Two pain points drove it:

- **qutebrowser's modal IME bug** (upstream #3444): it switches insert/normal but has
  no mode-switch hook, so the IME eats keys in normal mode; you can't cleanly "IME on
  only in insert".
- **Extension-layer control is incomplete**: Vimium/SurfingKeys can't inject into error
  pages (`chrome-error`) or built-in pages.

**Selection**: `chromium --app` + CDP + SurfingKeys + sqlite/mudra. A real Chromium
origin with an external controller (keeps SurfingKeys + error-page control), rather
than re-embedding an engine.

**Design hard constraint — nearly niri-exclusive**: the whole mode needs a WM with
(fine-grained IPC control: move/focus/set-column-width/workspace routing) **and** a
suitable workspace model. **niri is the best fit, for three structural reasons**:

1. **Dynamic, cheap workspaces**: isolation in mudra is *workspace-routed* — each
   context (situation leaf) gets its own workspace. niri workspaces are created on
   demand and cost nothing; bounded-workspace WMs force a fixed isolation budget.
2. **Fine-grained IPC** (`niri msg`): move/focus windows, route them to workspaces at
   spawn time, and read `layout.tile_size` + focused-output geometry — the latter is
   what makes per-site column-width memory possible (`col`).
3. **Scrollable column layout**: the single-column dev-tools stack (console beside
   pages) and the width-snap bands map 1:1 onto niri's column model.

hyprland qualifies on IPC but its bounded workspaces limit "arbitrary isolation
dimensions × multi-window layout"; cosmic-de lacks the required control surface.

---

## Content organization: tag forest *replaces session*

Pages are no longer bucketed under a single-owner `session`. They live in a **tag
forest** — many trees, no single root, trees don't exclude each other, within a tree
only one value (multi-owner across trees). Degenerate cases cover both ends (one-tag
trees → plain multi-tag; all tags on one tree → classic taxonomy).

- **`situation`** tree (`required`, single-select, default `inbox`): `inbox / work /
  personal / privacy` — the *current context*, decides isolation.
- **`importance` / `urgency`** trees (two-level, ranked): leaves `☆..☆☆☆☆☆`, `rank`
  field orders them. Scoring *is* a tree, not a separate field. A domain/subdomain rule
  table assigns default scores (e.g. infoq high-importance, its user-content subdomain
  low); manual overrides win. ML (naive-Bayes / logistic regression, not an LLM) can
  later *suggest* rules from your manual scores.
- **`topic`** trees (plain, multi-select): `project:x / news / tech`.

**Isolated instance**: a tag with `isolated=true` runs its content in an independent
instance — own profile/cookie/process (login/privacy isolation). Currently the four
`situation` leaves are isolated; a page matching several isolated tags lands on the
first match (unspecified; not special-cased — these are mutually exclusive anyway).

**Inbox flow**: new information defaults to `situation=inbox`; after processing it is
moved to the matching dimension (`work`/`personal`/…). **pin / PWA**: always-on content
(IM/RSS) lives in a dedicated constantly-running instance — a PWA-like always-on app
slot.

**Terminology**: *tab → page* (the strip-less browser has no tabs; everything is a
`page`), and *tag* (not label) for the tree nodes.

**Interaction surface**: the tag-forest rich operations (scoring axes, capsule tag
switching, batch assign) live in a solidjs management panel — `mudra ui`. The launcher
keeps only the `p` (Page) hot-path. Panel architecture & tag-forest abstraction roadmap:
`docs/PANEL.md`.

## Data model

```sql
tag(id, parent_id, name, alias, isolated, required, rank, hidden, note)
page_tag(page_id, tag_id)      -- multi-row = multi-tag; within-tree single-select is an app constraint
instances(id, profile, port, pid, running, proxy, extensions)  -- 1 isolated tag ↔ 1 instance
pages(id, instance_id FK, target_id, url, title, position, opened_at, closed_at)
site_widths(site, proportion)   -- per-site column-width memory
state(key, value)               -- current_context(situation), sort, ...
```

- Adjacency list (`parent_id`) + recursive CTE; root sentinel `parent_id = -1`.
- `state.current_context` replaces `current_session` (default `inbox`).
- Common query patterns (subtree, name-path→id with `input/v/g` stages, full-path
  names, join-by-tag, ensure-path upsert) are distilled from the `scratch` task tool —
  see `tag-forest.md`.

---

## Architecture (layers)

- **`--app` per page** — no omnibox / tab strip → maximal webview. `tag` (browser
  tabs) lives in mudra's tag forest, not in Chromium. CDP can't touch Chromium's UI
  chrome, so minimal UI comes only from `--app`.
- **CDP backbone** — controls every target (incl. error/built-in pages);
  `Target.targetCreated/Destroyed` events live-sync to sqlite; recovery.
- **SurfingKeys** — keyboard/insert/IME on normal pages (JS-driven); pre-seeded into
  each instance profile.
- **sqlite + launcher** — tag-forest org & refine, isolated instances, window↔process
  mapping, url record/filter, per-site column width; walker lists pages filtered by tag
  and does address input.

**Engine control — verified**: chromium is the only engine with a mature **CDP**.
Ladybird/Servo both have remote control but over the **Firefox RDP** (actor/TCP JSON),
not CDP, and it's partial — not a daily-driver surface. mudra's control layer abstracts
a `BrowserEngine` interface (chromium=CDP backend) so another engine can slot in later.

## Instances, isolation & workspaces

- Process-per-page; one instance per isolated tag dimension (own profile/cache) — too
  few instances; cost is a few extra browser processes for hard cookie/privacy isolation.
- Each isolated instance binds a niri **workspace**; `mudra open` focuses it + spawns +
  rebuilds. Windows land there via niri window-rule (title/app_id) + focus-workspace.
- Mixed windows can share a workspace; moving a process's windows to another workspace
  is the "separate" operation (`mudra move`).

---

## Quick start

```bash
# 1. daemon (live-sync sqlite with real windows; owns ALL lifecycle ops)
python3 mudrad.py run

# 2. open a page in the current context (spawns a chromium --app instance for it)
python3 mudra.py open <url>

# 3. add / close pages; switch context
python3 mudra.py add <url>
python3 mudra.py close [query]
python3 mudra.py ctx [leaf]        # show or switch current situation leaf

# 4. list pages per context
python3 mudra.py ls
```

All lifecycle verbs are thin HTTP clients of the daemon's control API
(`http://127.0.0.1:8899`) — the daemon is the single point that spawns/kills windows
and writes sqlite; the panel talks to the same API over WebSocket.

## Commands

| command | what it does |
|---|---|
| `mudra.py open <url> [--ctx LEAF]` | open a page in a context (spawns its instance if not running) |
| `mudra.py add <url> [--bg] [--ctx LEAF]` | add a page to the context's running instance (`--bg` keeps focus) |
| `mudra.py close [query] [--ctx LEAF]` | close the whole context instance, or one open page (url filter) |
| `mudra.py ctx [LEAF]` | show / switch the current context (situation leaf) |
| `mudra.py ls [-f FILTER]` | list open pages per context (URL/title filter) |
| `mudra.py targets [--ctx LEAF]` | list live page targets (CDP) |
| `mudra.py focus <query> [--ctx LEAF]` | find a page by url/title and bring it forward |
| `mudra.py goto/back/forward/reload <url>` | navigation |
| `mudra.py move <workspace>` | move the context's windows to a workspace (niri) |
| `mudra.py conf <leaf> [--proxy <p>] [--ext <csv>]` | per-context proxy/extensions (applied on next open/add) |
| `mudra.py col remember\|show` | remember/per-site apply column width (niri) |
| `mudra.py ui` | open the solidjs web console — the tag-forest management surface |
| `mudrad.py run` | daemon: control API + WebSocket push + Target→sqlite live-sync |

## Environment

- To launch Chromium **windows** the shell needs the Wayland env:
  `export WAYLAND_DISPLAY=wayland-1 XDG_RUNTIME_DIR=/run/user/1000`.
- niri socket auto-discovered (`NIRI_SOCKET` or `/run/user/<uid>/niri.wayland-*.sock`).
- `python3` (stdlib only), `chromium`, `niri`.

## Window management (walker)

- **Unified switching**: each mudra page window is a plain niri window and appears
  directly in Alt+Tab — no in-browser secondary switching layer. Extra windows are a
  side effect; filter them in the launcher when needed. Alt+Tab remains the hot path.
- **No title prefix, no exclusion**: the earlier plan (title prefix to filter/exclude
  browser windows from generic switchers) is dropped — a dedicated mudra window menu
  is no longer needed.
- **Mod+Space (Win+Space)** opens the mudra **web console** (`mudra ui`), which shows
  only mudra pages. Instances record address / pid / window id at spawn, so no title
  prefix is required for identification.
- **No launcher work needed**: the console is the entire management surface — search/
  filter, operations (focus/close/move…), and tag-forest tree view all live there.
  Launcher side stays untouched (only the Mod+Space binding). The earlier `p` hot-path
  mode and `p/t/a/s` prefixes are dropped.

## Implementation notes / details

- Launched: `chromium --app=<url> --remote-debugging-port=<dyn> --user-data-dir=<profile>
  --no-first-run`. Proxy/extension flags come from the context's `conf` row
  (`proxy` / `extensions` columns) and are applied at spawn.
- **SurfingKeys must be an MV3 build**: Chromium 139+ rejects `--load-extension` of
  MV2 unpacked dirs ("unsupported manifest version" — silently dropped unless you
  chase stderr). The repo-era 1.15.0 crx is MV2; build the `mv3` branch
  (Surfingkeys 1.17.x) into `~/.local/share/mudra/extensions/surfingkeys/`.
- **New-window interception (cascade)**: in `--app`, `_blank`/`window.open` open as a
  chrome-default window; qwd injects a script (overrides `window.open` +
  capture `a[target=_blank]` → Image beacon → local `/open` → a new `--app`). Must be
  (re)injected on every new target (`Target.targetCreated` →
  `Page.addScriptToEvaluateOnNewDocument`), else a window qwd spawns carries no script.
- **Close semantics**: mudra-initiated close deletes the page; WM/accidental close
  keeps it (marks `closed_at`) — active close leaves no trace, external close loses
  nothing.
- **Column width** (verified): read `layout.tile_size[0]` ÷ `focused-output.logical.width`;
  set `set-column-width <N%>` (percent; `1/2` fraction syntax errors). Snap to a band,
  store per-site; re-apply on open.
- **Daemon** is a singleton (flock) and idempotent (`UNIQUE(instance_id,target_id)` +
  `INSERT OR IGNORE`) to survive races.
- **Stale singleton trap**: a leftover chromium from a previous schema still holds the
  profile's `SingletonLock`; a new spawn hands its flags over to the old process and
  exits — the debug port never listens and the daemon marks the instance down. If
  instances go "never ready" after a profile/schema change, kill the old
  `mudra/profiles/*` chromium processes first.

## Status & open questions

**Verified**: `--app` w/o chrome, CDP lists/attaches `--app`, niri
move/focus/column-width, `--load-extension`, walker multi-char prefixes + `argument_delimiter`.
**Pending/⏳**: tag-forest migration (situation switch, isolated instances, scoring
trees, ML), SurfingKeys-in-app, CDP error-page nav, walker provider protocol (P7b).

**Known issue (deferred)**: passwords aren't saved in `--app` (Linux keyring absent) —
candidates: `gnome-keyring` or `--password-store=basic`.

**Open**: dual control routing (SurfingKeys vs CDP for error pages) · download/print
(chrome UI gaps) · tag-forest app-layer single-select enforcement · ML scoring.

## Roadmap

See `PLAN.md` — P4/P5 done (proxy+extensions, column memory), P6 done (new-window
interception), P7a done (WmExt interface + niri backend), **P7b** (walker launcher
menus via elephant `menus` provider), then the **tag-forest architecture** (§9).