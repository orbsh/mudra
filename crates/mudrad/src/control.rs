//! Control verbs — the port of `mudrad.py`'s `_InterceptHandler` +
//! `ops.py`: the single execution point for window/instance/page
//! lifecycle. JSON in, JSON out; the HTTP shell in `daemon.rs` binds
//! `POST <verb>` to `Controller::handle`.
//!
//! Division of labor (the Python layering kept): state transitions live
//! in `mudra_store::lifecycle`, machine effects behind the `Runtime`
//! seam, this layer only orchestrates and answers. Every verb that
//! writes state returns the bumped epoch through `Runtime::notify` —
//! the panel's invalidation hint cannot drift from the write.

use serde_json::{json, Value};

use mudra_store::{MudraStore, state};

use crate::runtime::Runtime;
use crate::spawn;

/// Port of the console panel; role detection for the extension bar
/// (backend decides, the frontend never guesses — Python rule).
pub const PANEL_PORT: u16 = 9299;

pub struct Controller<'a, R: Runtime> {
    pub store: &'a mut MudraStore,
    pub rt: &'a mut R,
}

impl<'a, R: Runtime> Controller<'a, R> {
    pub fn handle(&mut self, path: &str, body: &Value) -> Result<Value, String> {
        match path {
            "/open" => self.open(body),
            "/add" => self.add(body),
            "/close_page" => self.close_page(body),
            "/close_ctx" => self.close_ctx(body),
            "/ctx" => self.set_ctx(body),
            "/tag" => self.tag(body),
            "/tags" => self.tags(body),
            "/pages" => self.pages(body),
            "/focus_page" => self.focus_page(body),
            "/ctx_status" => self.ctx_status(body),
            // R2 slice 2: the panel's control-plane verbs. Reads that
            // need server-side shaping (forest, ctx_pages, shot) and
            // every write ride 8899 — the KV frame channel is the
            // read-only data plane; a frame PUT would bypass the
            // Collection's index/epoch maintenance (方案 A).
            "/forest" => self.forest(),
            "/ctx_pages" => self.ctx_pages(body),
            "/set_tags" => self.set_tags(body),
            "/create_tag" => self.create_tag(body),
            "/close" => self.close_by_id(body),
            "/reopen" => self.reopen(body),
            "/delete" => self.delete(body),
            "/shot" => self.shot(body),
            // A2: the CLI's fact source — every remaining Python mudra.py
            // command lands here so the CLI can become a pure 8899
            // forwarder (iron rule: zero DB writes, zero process ops).
            "/ctx_current" => Ok(json!({"ctx": self.current_ctx()})),
            "/contexts" => self.contexts(),
            "/targets" => self.targets(body),
            "/focus" => self.focus(body),
            "/nav" => self.nav(body),
            "/page_action" => self.page_action(body),
            "/move" => self.move_ctx(body),
            "/col" => self.col(body),
            "/conf" => self.conf(body),
            "/dev" => self.dev(body),
            "/tag_seed" => self.tag_seed(),
            "/tag_set" => self.tag_set(body),
            "/sort" => self.sort(body),
            "/panel" => self.panel(body),
            other => Err(format!("unknown endpoint {other}")),
        }
    }

    fn current_ctx(&self) -> String {
        let t = self.store.state_text(state::CURRENT_CONTEXT);
        if t.is_empty() { "inbox".into() } else { t } // DEFAULT_CONTEXT
    }

    /// tabId -> owning ctx: the extension's chrome tabId is not a CDP
    /// targetId (Python lesson), so probe each running instance's /json
    /// first, then fall back to URL reverse lookup.
    fn ctx_for_tab(&mut self, tab_id: &str, url: Option<&str>) -> Option<String> {
        if !tab_id.is_empty() {
            for (k, inst) in self.running_instances() {
                let _ = k;
                if self.rt.target_exists(inst.port as u16, tab_id) {
                    return Some(inst.profile);
                }
            }
        }
        self.ctx_for_url(url?)
    }
    /// URL -> ctx when the owning instance is unambiguous among open pages.
    fn ctx_for_url(&self, url: &str) -> Option<String> {
        let prefix = url.split('#').next().unwrap_or(url);
        let mut profiles: Vec<String> = Vec::new();
        for (k, inst) in self.running_instances() {
            for (_, p) in self.store.pages_of_instance(k.id, false) {
                if p.closed_at == 0
                    && p.url.starts_with(prefix)
                    && !profiles.contains(&inst.profile)
                {
                    profiles.push(inst.profile.clone());
                }
            }
        }
        (profiles.len() == 1).then(|| profiles.remove(0))
    }

    fn running_instances(&self) -> Vec<(mudra_store::InstanceKey, mudra_store::Instance)> {
        self.store
            .instances
            .scan_keys()
            .into_iter()
            .filter_map(|k| self.store.instances.get(&k).map(|i| (k, i)))
            .filter(|(_, i)| i.running == 1 && spawn::pid_alive(i.pid)) // zombie-safe
            .collect()
    }

    /// The live instance row for a context (running=1 AND pid actually
    /// alive — the join-path blood lesson: DB flags drift, /proc decides).
    fn alive_instance_for(&self, ctx: &str) -> Option<(mudra_store::InstanceKey, mudra_store::Instance)> {
        self.store
            .instance_for_context(ctx)
            .filter(|(_, i)| i.running == 1 && spawn::pid_alive(i.pid))
    }

    // ================= verbs =================

    /// POST /open {url, ctx?, tabId?, proxy?, extensions?}
    /// Instance alive -> join as --app; dead -> new instance (debug port,
    /// proxy/extensions reused from the old row).
    pub fn open(&mut self, body: &Value) -> Result<Value, String> {
        let raw = body.get("url").and_then(Value::as_str).unwrap_or_default();
        if raw.is_empty() {
            return Err("need url".into());
        }
        let url = spawn::normalize_url(raw);
        let ctx = match body.get("ctx").and_then(Value::as_str) {
            Some(c) => c.to_string(),
            None => self.ctx_for_tab(
                tab_text(body.get("tabId")).as_str(),
                body.get("url").and_then(Value::as_str),
            )
            .unwrap_or_else(|| self.current_ctx()),
        };
        let inst = self.store.instance_for_context(&ctx);
        // alive? join. dead? new instance reusing the old row's config.
        let alive = inst.as_ref().filter(|(_, i)| i.running == 1 && spawn::pid_alive(i.pid));
        if let Some((_, i)) = alive {
            let (port, proxy, ext) = (i.port as u16, or_none(&i.proxy), or_none(&i.extensions));
            self.rt.launch_join(&ctx, &url, proxy, ext).map_err(|e| e.to_string())?;
            return Ok(json!({"mode": "joined", "port": port, "ctx": ctx}));
        }
        // new instance: reuse the old row's config when present
        let (proxy, ext) = inst
            .as_ref()
            .map(|(_, i)| (or_none(&i.proxy).map(str::to_string), or_none(&i.extensions).map(str::to_string)))
            .unwrap_or((None, None));
        let reuse = inst.as_ref().map(|(k, _)| k.clone());
        let port = spawn::free_port(9200, 200).map_err(|e| e.to_string())?;
        let pid = self.rt.launch_new(&ctx, &url, port, proxy.as_deref(), ext.as_deref())?;
        self.store.launch_started(reuse.as_ref(), &ctx, port as u32, pid, proxy.as_deref(), ext.as_deref());
        // remembered site width: spawn side effect (blocking, 3s budget)
        if let Some((_, sw)) = self.store.site_width(&mudra_store::url_site(&url)) {
            self.rt.apply_site_width(pid, sw.proportion.0);
        }
        Ok(json!({"mode": "new", "port": port, "pid": pid, "ctx": ctx}))
    }

    /// POST /add {url, ctx?} — join only; never silently spawns.
    pub fn add(&mut self, body: &Value) -> Result<Value, String> {
        let raw = body.get("url").and_then(Value::as_str).unwrap_or_default();
        if raw.is_empty() {
            return Err("need url".into());
        }
        let url = spawn::normalize_url(raw);
        let ctx = body.get("ctx").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| self.current_ctx());
        let (_, inst) = self
            .alive_instance_for(&ctx)
            .ok_or(format!("ctx {ctx:?} not running; use open first"))?;
        self.rt.launch_join(&ctx, &url, or_none(&inst.proxy), or_none(&inst.extensions))?;
        Ok(json!({"mode": "joined", "port": inst.port, "ctx": ctx}))
    }

    /// POST /close_page {query, ctx?} — close the CDP target only; the
    /// watcher's destroyed event performs the row teardown (single path).
    pub fn close_page(&mut self, body: &Value) -> Result<Value, String> {
        let query = body.get("query").and_then(Value::as_str).unwrap_or_default();
        if query.is_empty() {
            return Err("need query".into());
        }
        let ctx = body.get("ctx").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| self.current_ctx());
        let (k, inst) = self
            .alive_instance_for(&ctx)
            .ok_or(format!("ctx {ctx:?} not running"))?;
        let hit = self
            .store
            .pages_of_instance(k.id, false)
            .into_iter()
            .find(|(_, p)| p.closed_at == 0 && (p.url.contains(query) || p.title.contains(query)))
            .ok_or(format!("no open page in ctx {ctx:?} matching {query:?}"))?;
        self.rt.close_target(inst.port as u16, &hit.1.target_id)?;
        Ok(json!({"closed": hit.1.url}))
    }

    /// POST /close_ctx {ctx?} — SIGTERM the instance; watcher teardown follows.
    pub fn close_ctx(&mut self, body: &Value) -> Result<Value, String> {
        let ctx = body.get("ctx").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| self.current_ctx());
        let (_, inst) = self
            .alive_instance_for(&ctx)
            .ok_or(format!("ctx {ctx:?} not running"))?;
        self.rt.kill(inst.pid)?;
        Ok(json!({"closed": ctx}))
    }

    /// POST /ctx {ctx} — switch the current context (situation leaf check
    /// in the store; bump notifies the panel header).
    pub fn set_ctx(&mut self, body: &Value) -> Result<Value, String> {
        let ctx = body.get("ctx").and_then(Value::as_str).unwrap_or_default();
        match self.store.set_context(ctx) {
            Some(epoch) => {
                self.rt.notify(epoch);
                Ok(json!({"ctx": ctx}))
            }
            None => Err(format!("not a situation leaf: {ctx:?}")),
        }
    }

    /// POST /tag {tabId?, url?, tag} — toggle on the resolved open page.
    pub fn tag(&mut self, body: &Value) -> Result<Value, String> {
        let tag_name = body.get("tag").and_then(Value::as_str).unwrap_or_default();
        if tag_name.is_empty() {
            return Err("need tag".into());
        }
        let url = body.get("url").and_then(Value::as_str).unwrap_or_default();
        let tab_id = tab_text(body.get("tabId"));
        let ctx = self
            .ctx_for_tab(&tab_id, if url.is_empty() { None } else { Some(url) })
            .ok_or("need tabId and url to resolve page")?;
        let (tk, _) = self
            .store
            .tag_by_name(tag_name)
            .into_iter()
            .next()
            .ok_or(format!("tag not found: {tag_name}"))?;
        let (k, _) = self
            .alive_instance_for(&ctx)
            .ok_or(format!("no instance for ctx {ctx}"))?;
        let prefix = url.split('#').next().unwrap_or(url);
        let (pk, _) = self
            .store
            .pages_of_instance(k.id, false)
            .into_iter()
            .filter(|(_, p)| p.closed_at == 0 && p.url.starts_with(prefix))
            .max_by_key(|(k, _)| k.id)
            .ok_or("page not open in this ctx")?;
        let added = self.store.page_tag_toggle(&pk, &tk);
        self.rt.notify(self.store.epoch());
        Ok(json!({"tag": tag_name, "action": if added { "added" } else { "removed" }}))
    }

    /// POST /tags {parent?} — tree drilling data source.
    pub fn tags(&mut self, body: &Value) -> Result<Value, String> {
        let parent = body.get("parent").and_then(Value::as_str);
        let children = match parent {
            Some(name) => self
                .store
                .tag_by_name(name)
                .into_iter()
                .next()
                .map(|(k, _)| self.store.tag_children(k.id as i32))
                .unwrap_or_default(),
            None => self.store.tag_children(-1),
        };
        Ok(json!({"tags": children.into_iter().map(|(_, t)| t.name).collect::<Vec<_>>()}))
    }

    /// POST /pages {ctx?} — open-page list (across contexts).
    pub fn pages(&mut self, body: &Value) -> Result<Value, String> {
        let ctx = body.get("ctx").and_then(Value::as_str);
        let mut out = Vec::new();
        for (k, inst) in self.running_instances() {
            if ctx.is_some_and(|c| c != inst.profile) {
                continue;
            }
            for (pk, p) in self.store.pages_of_instance(k.id, false) {
                if p.closed_at == 0 {
                    out.push(json!({"id": pk.id, "title": p.title, "url": p.url, "ctx": inst.profile}));
                }
            }
        }
        Ok(json!({"pages": out}))
    }

    /// POST /focus_page {page_id} — CDP activate + niri bring-forward.
    /// A switch counts only when both happen (Python contract: niri
    /// failure must not block the CDP activate).
    pub fn focus_page(&mut self, body: &Value) -> Result<Value, String> {
        let page_id = body.get("page_id").and_then(Value::as_u64).unwrap_or(0);
        let pk = mudra_store::PageKey { id: page_id };
        let page = self
            .store
            .pages
            .get(&pk)
            .filter(|p| p.closed_at == 0 && p.deleted_at == 0)
            .ok_or(format!("page {page_id} not found"))?;
        let inst = self
            .store
            .instances
            .get(&mudra_store::InstanceKey { id: page.instance_id })
            .filter(|i| i.running == 1 && spawn::pid_alive(i.pid))
            .ok_or("instance down")?;
        self.rt.activate_target(inst.port as u16, &page.target_id)?;
        self.rt.focus_window(inst.pid, &page.title, &page.url); // best effort
        Ok(json!({"focused": page_id}))
    }

    /// POST /ctx_status {tabId?, url?} — the extension bar data source.
    pub fn ctx_status(&mut self, body: &Value) -> Result<Value, String> {
        let url = body.get("url").and_then(Value::as_str).unwrap_or_default().to_string();
        let tab_id = tab_text(body.get("tabId"));
        let role = if url.starts_with(&format!("http://127.0.0.1:{PANEL_PORT}/")) {
            "console"
        } else {
            "page"
        };
        if role == "console" {
            return Ok(json!({"ctx": Value::Null, "tags": [], "capsules": "", "role": role}));
        }
        let ctx = self
            .ctx_for_tab(&tab_id, if url.is_empty() { None } else { Some(&url) })
            .or_else(|| self.ctx_for_url(&url));
        let mut tags: Vec<String> = Vec::new();
        let prefix = url.split('#').next().unwrap_or(&url);
        if let Some(ref c) = ctx
            && let Some((k, _)) = self.store.instance_for_context(c)
            && let Some((pk, _)) = self
                .store
                .pages_of_instance(k.id, false)
                .into_iter()
                .filter(|(_, p)| p.closed_at == 0 && p.url.starts_with(prefix))
                .max_by_key(|(k, _)| k.id)
        {
            for tk in self.store.tags_of_page(&pk) {
                tags.push(self.store.tag_path_string(&tk));
            }
        }
        // SSR capsules: the bar drops this string in verbatim (the tag
        // type component lives once, in the tag-forest crate — content
        // scripts cannot compile wasm under page CSP). `tags` stays for
        // the cmd-palette drill-down, which consumes paths, not HTML.
        let owned: Vec<&str> = tags.iter().map(String::as_str).collect();
        let capsules = tag_forest::capsules_html(&owned);
        Ok(json!({"ctx": ctx, "tags": tags, "capsules": capsules, "role": role}))
    }

    // ============ R2 slice 2: panel control-plane verbs ============

    /// POST /forest — the whole tag forest, recursive, plus the context
    /// switcher data (port of ui.py `_forest` + `_contexts`). Shapes are
    /// the panel's existing contract: roots carry `root: true` and
    /// `rank_axis` (the glyph table lives in the tag-forest crate, one
    /// source for SSR bar, panel and mudrad); non-roots carry `path`
    /// (`::`-joined) and `rank` — `0` (unranked) marshals as JSON null,
    /// because the panel discriminates plain tags by `rank === null` and
    /// selects rank nodes by `rank === k`; the okm row has no null.
    pub fn forest(&self) -> Result<Value, String> {
        /// One live non-root tag row, flattened for tree assembly:
        /// (id, name, alias, rank, isolated, required).
        type Kid = (u32, String, String, i32, u8, u8);
        type KidMap = std::collections::HashMap<i64, Vec<Kid>>;

        // all live rows, id-ordered (Python's ORDER BY id: children keep
        // insertion order, not rank order — the rank sort is the panel's)
        let mut rows: Vec<(mudra_store::TagKey, mudra_store::Tag)> = self
            .store
            .tags
            .scan_keys()
            .into_iter()
            .filter_map(|k| self.store.tags.get(&k).filter(|t| t.deleted == 0).map(|t| (k, t)))
            .collect();
        rows.sort_by_key(|(k, _)| k.id);

        let mut by_parent: KidMap = std::collections::HashMap::new();
        let mut roots: Vec<Value> = Vec::new();
        for (k, t) in &rows {
            if t.parent_id == -1 {
                roots.push(json!({
                    "id": k.id, "name": t.name, "alias": t.alias,
                    "root": true, "rank_axis": tag_forest::root_axis(&t.name),
                    "children": [],
                }));
            } else {
                by_parent.entry(t.parent_id as i64).or_default().push((
                    k.id, t.name.clone(), t.alias.clone(), t.rank, t.isolated, t.required,
                ));
            }
        }
        fn build(kid: Kid, prefix: &str, by_parent: &KidMap) -> Value {
            let (id, name, alias, rank, isolated, required) = kid;
            let path = format!("{prefix}::{name}");
            let children: Vec<Value> = by_parent
                .get(&(id as i64))
                .map(|kids| kids.iter().cloned().map(|k| build(k, &path, by_parent)).collect())
                .unwrap_or_default();
            json!({
                "id": id, "name": name, "alias": alias, "path": path,
                "rank": if rank == 0 { Value::Null } else { json!(rank) },
                "isolated": isolated == 1, "required": required == 1,
                "children": children,
            })
        }
        for root in &mut roots {
            let rid = root["id"].as_u64().unwrap_or(0) as i64;
            let rname = root["name"].as_str().unwrap_or_default().to_string();
            if let Some(kids) = by_parent.get(&rid) {
                let built: Vec<Value> =
                    kids.iter().cloned().map(|k| build(k, &rname, &by_parent)).collect();
                root["children"] = json!(built);
            }
        }
        // situation leaves in id order (Python `_contexts` ORDER BY t.id);
        // the current context resolves exactly like the panel's `load()`
        // default so one round trip carries the full header state.
        let contexts: Vec<String> = {
            let sit_root = rows
                .iter()
                .find(|(_, t)| t.parent_id == -1 && t.name == "situation")
                .map(|(k, _)| k.id as i64);
            match sit_root {
                Some(rid) => by_parent
                    .get(&rid)
                    .map(|kids| {
                        let mut names: Vec<(u32, String)> =
                            kids.iter().map(|(id, n, ..)| (*id, n.clone())).collect();
                        names.sort_by_key(|(id, _)| *id);
                        names.into_iter().map(|(_, n)| n).collect()
                    })
                    .unwrap_or_default(),
                None => Vec::new(),
            }
        };
        Ok(json!({
            "forest": roots,
            "contexts": contexts,
            "current": self.current_ctx(),
        }))
    }

    /// POST /ctx_pages {ctx?} — every undeleted page of a context,
    /// closed ones included (the panel strikes them through and offers
    /// ↻/🗑). Port of ui.py `_pages`: position-ordered, title falls back
    /// to url, tag_ids ride along so rank/capsule state needs no extra
    /// round trip.
    pub fn ctx_pages(&self, body: &Value) -> Result<Value, String> {
        let ctx = body
            .get("ctx")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| self.current_ctx());
        let mut out = Vec::new();
        for (ik, inst) in self.store.instance_by_profile(&ctx) {
            let mut pages: Vec<(mudra_store::PageKey, mudra_store::Page)> =
                self.store.pages_of_instance(ik.id, false);
            pages.sort_by_key(|(_, p)| p.position);
            for (pk, p) in pages {
                let tag_ids: Vec<u32> = self
                    .store
                    .tags_of_page(&pk)
                    .into_iter()
                    .map(|tk| tk.id)
                    .collect();
                out.push(json!({
                    "id": pk.id,
                    "url": p.url,
                    "title": if p.title.is_empty() { p.url.clone() } else { p.title.clone() },
                    "position": p.position,
                    "tag_ids": tag_ids,
                    "target_id": p.target_id,
                    "parent_id": p.parent_id,
                    "opened_at": p.opened_at,
                    "closed": p.closed_at != 0,
                    "port": inst.port,
                }));
            }
        }
        Ok(json!({"pages": out}))
    }

    /// POST /set_tags {page_id, tag_ids} — replace the page's whole tag
    /// set (rank picks, capsule switches, add/remove all land here; the
    /// Python `deleted=0` filter lives in the store method).
    pub fn set_tags(&mut self, body: &Value) -> Result<Value, String> {
        let page_id = body.get("page_id").and_then(Value::as_u64).unwrap_or(0);
        let pk = mudra_store::PageKey { id: page_id };
        self.store
            .pages
            .get(&pk)
            .filter(|p| p.deleted_at == 0)
            .ok_or(format!("page {page_id} not found"))?;
        let ids: Vec<u32> = body
            .get("tag_ids")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_u64().map(|n| n as u32)).collect())
            .unwrap_or_default();
        let epoch = self.store.page_tag_replace(&pk, &ids);
        self.rt.notify(epoch);
        Ok(json!({"page_id": page_id, "tag_ids": ids}))
    }

    /// POST /create_tag {parent_id, name} — idempotent under (parent,
    /// name); returns the node id, created or found (Python parity).
    pub fn create_tag(&mut self, body: &Value) -> Result<Value, String> {
        let name = body
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if name.is_empty() {
            return Err("tag name required".into());
        }
        let parent_id = body.get("parent_id").and_then(Value::as_i64).unwrap_or(-1) as i32;
        let (k, created) = self.store.create_tag(parent_id, name);
        // idempotent-found is a no-op: silent, like every other verb
        // (the store only bumps when it writes)
        if created {
            self.rt.notify(self.store.epoch());
        }
        Ok(json!({"id": k.id}))
    }

    /// POST /close {page_id} — set closed_at AND close the target
    /// (ops.close_page port). The row survives; the watcher's destroyed
    /// event re-marks idempotently (the closed_at==0 guard).
    pub fn close_by_id(&mut self, body: &Value) -> Result<Value, String> {
        let page_id = body.get("page_id").and_then(Value::as_u64).unwrap_or(0);
        let pk = mudra_store::PageKey { id: page_id };
        // ops.close_page raised on a missing row — keep that; a row that
        // is already closed is a legitimate no-op (double ⨯ click).
        if self.store.pages.get(&pk).is_none() {
            return Err(format!("page {page_id} not found"));
        }
        // port order: closed_at first (Python UPDATEs, then closes the
        // target) — a live port/target is best effort like Python's guard
        if let Some((target_id, _epoch)) = self.store.close_page(&pk, crate::watch::now_ms()) {
            let live = self
                .store
                .pages
                .get(&pk)
                .and_then(|p| self.store.instances.get(&mudra_store::InstanceKey { id: p.instance_id }))
                .filter(|i| i.running == 1 && spawn::pid_alive(i.pid));
            if let Some(inst) = live {
                let _ = self.rt.close_target(inst.port as u16, &target_id);
            }
            self.rt.notify(self.store.epoch());
        }
        Ok(json!({"closed": page_id}))
    }

    /// POST /reopen {page_id} — re-open a closed row's URL through the
    /// normal open path (ops.open_page port: `ctl_open(url, ctx)`); the
    /// watcher's upsert revives the row when the new target lands.
    pub fn reopen(&mut self, body: &Value) -> Result<Value, String> {
        let page_id = body.get("page_id").and_then(Value::as_u64).unwrap_or(0);
        let pk = mudra_store::PageKey { id: page_id };
        let page = self
            .store
            .pages
            .get(&pk)
            .ok_or(format!("page {page_id} not found"))?;
        if page.deleted_at != 0 {
            return Err("page is deleted".into());
        }
        let ctx = self
            .store
            .instances
            .get(&mudra_store::InstanceKey { id: page.instance_id })
            .map(|i| i.profile)
            .filter(|c| !c.is_empty())
            .ok_or(format!("page {page_id} has no context instance"))?;
        let url = page.url.clone();
        self.open(&json!({"url": url, "ctx": ctx}))?;
        Ok(json!({"opened": page_id}))
    }

    /// POST /delete {page_id} — soft delete, closed rows only (the
    /// invariant lives in `lifecycle::delete_page`; the verb just shells
    /// it and pushes the epoch).
    pub fn delete(&mut self, body: &Value) -> Result<Value, String> {
        let page_id = body.get("page_id").and_then(Value::as_u64).unwrap_or(0);
        let pk = mudra_store::PageKey { id: page_id };
        let epoch = self.store.delete_page(&pk, crate::watch::now_ms())?;
        self.rt.notify(epoch);
        Ok(json!({"deleted": page_id}))
    }

    /// POST /shot {page_id} — hover screenshot (port of ui.py `_shot`):
    /// live page only, None answer when the page/instance/target is not
    /// there right now (Python returned data: null, not an error).
    pub fn shot(&mut self, body: &Value) -> Result<Value, String> {
        let page_id = body.get("page_id").and_then(Value::as_u64).unwrap_or(0);
        let pk = mudra_store::PageKey { id: page_id };
        let data = match self
            .store
            .pages
            .get(&pk)
            .filter(|p| p.closed_at == 0 && p.deleted_at == 0 && !p.target_id.is_empty())
            .and_then(|p| {
                self.store
                    .instances
                    .get(&mudra_store::InstanceKey { id: p.instance_id })
                    .filter(|i| i.port > 0 && i.running == 1)
                    .map(|i| (i.port as u16, p.target_id.clone()))
            })
        {
            Some((port, target)) => self.rt.screenshot(port, &target)?,
            None => None,
        };
        Ok(json!({"data": data}))
    }
    // ================= A2: CLI fact-source verbs =================

    /// POST /contexts {} — situation leaves (id order) + open page count
    /// per leaf + the current one (port of `mudra ls`'s overview SQL).
    /// The overview counts OPEN pages, deleted excluded (Python rule:
    /// every read path carries the filter).
    pub fn contexts(&self) -> Result<Value, String> {
        let sit = self
            .store
            .tag_by_name("situation")
            .into_iter()
            .find(|(_, t)| t.parent_id == -1)
            .map(|(k, _)| k.id);
        let leaves = sit.map(|id| self.store.tag_children(id as i32)).unwrap_or_default();
        let current = self.current_ctx();
        let mut out = Vec::new();
        for (k, t) in leaves {
            let _ = k;
            let n = self
                .store
                .instance_by_profile(&t.name)
                .into_iter()
                .map(|(ik, _)| {
                    self.store
                        .pages_of_instance(ik.id, false)
                        .into_iter()
                        .filter(|(_, p)| p.closed_at == 0)
                        .count()
                })
                .max()
                .unwrap_or(0);
            out.push(json!({"leaf": t.name, "pages": n, "current": t.name == current}));
        }
        Ok(json!({"contexts": out}))
    }

    /// POST /targets {ctx} — live page targets of the ctx instance (CDP
    /// passthrough, port of `mudra targets`; no DB read — the answer is
    /// whatever chromium says right now).
    pub fn targets(&mut self, body: &Value) -> Result<Value, String> {
        let ctx = body.get("ctx").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| self.current_ctx());
        let (_, inst) = self.alive_instance_for(&ctx).ok_or(format!("ctx {ctx:?} not running"))?;
        let rows = self.rt.list_targets(inst.port as u16)?;
        Ok(json!({"targets": rows.iter().map(|t| json!({
            "targetId": t["id"], "title": t["title"], "url": t["url"],
        })).collect::<Vec<_>>()}))
    }

    /// POST /focus {query, ctx?} — the CLI focus verb (ops.focus_ctx_query
    /// port): fuzzy-match live CDP pages in the ctx, map the hit's
    /// targetId to a stored row, then run the exact same path as
    /// /focus_page (CDP activate + niri bring-forward).
    pub fn focus(&mut self, body: &Value) -> Result<Value, String> {
        let query = body.get("query").and_then(Value::as_str).unwrap_or_default();
        if query.is_empty() {
            return Err("need query".into());
        }
        let ctx = body.get("ctx").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| self.current_ctx());
        let (ik, inst) = self.alive_instance_for(&ctx).ok_or(format!("ctx {ctx:?} not running"))?;
        let q = query.to_lowercase();
        let hit = self
            .rt
            .list_targets(inst.port as u16)?
            .into_iter()
            .find(|t| {
                let hay = format!("{} {}", t["url"].as_str().unwrap_or_default(), t["title"].as_str().unwrap_or_default()).to_lowercase();
                hay.contains(&q)
            })
            .ok_or(format!("no page matching {query:?}"))?;
        let target = hit["id"].as_str().unwrap_or_default();
        let (pk, page) = self
            .store
            .page_by_target(target)
            .filter(|(_, p)| p.instance_id == ik.id)
            .ok_or("page not recorded (sync pending?)")?;
        self.focus_page(&json!({"page_id": pk.id}))?;
        // the label mirrors Python's print: title, falling back to url
        let label = if page.title.is_empty() { page.url.clone() } else { page.title.clone() };
        Ok(json!({"focused": pk.id, "label": label}))
    }

    /// POST /nav {cmd, ctx?, url?} — Page-domain navigation on the ctx's
    /// rightmost open page (ctl.current_target_id rule: running instance,
    /// closed excluded, position DESC). cmd: goto|back|forward|reload.
    /// Navigation mutates no stored rows — the watcher's metadata events
    /// sync the new URL (single write path stays).
    pub fn nav(&mut self, body: &Value) -> Result<Value, String> {
        let cmd = body.get("cmd").and_then(Value::as_str).unwrap_or_default();
        let (method, params): (&str, Value) = match cmd {
            "goto" => {
                let raw = body.get("url").and_then(Value::as_str).unwrap_or_default();
                if raw.is_empty() {
                    return Err("goto needs url".into());
                }
                ("Page.navigate", json!({"url": spawn::normalize_url(raw)}))
            }
            "back" | "forward" => ("mudra.history", json!({"delta": if cmd == "back" { -1 } else { 1 } })),
            "reload" => ("Page.reload", json!({"ignoreCache": false})),
            other => return Err(format!("unknown nav cmd {other:?}")),
        };
        let ctx = body.get("ctx").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| self.current_ctx());
        let (ik, inst) = self.alive_instance_for(&ctx).ok_or(format!("ctx {ctx:?} not running"))?;
        let page = self
            .store
            .pages_of_instance(ik.id, false)
            .into_iter()
            .filter(|(_, p)| p.closed_at == 0 && !p.target_id.is_empty())
            .max_by_key(|(_, p)| p.position)
            .map(|(_, p)| p)
            .ok_or(format!("ctx {ctx:?} has no open page"))?;
        let answer = if method == "mudra.history" {
            // two-step, the ctl._history_step port: read the nav history,
            // jump to index ± 1 (Python semantics: out-of-range entry ids
            // just fail on the chromium side, no clamping guesswork)
            let hist = self
                .rt
                .page_command(inst.port as u16, &page.target_id, "Page.getNavigationHistory", json!({}))?
                .ok_or("page not live")?;
            // page_command answers the envelope's `result` object (the
            // seam strips the {id,result} wrapper once — indexing
            // ["result"] again here was a live-fire double unwrap)
            let idx = hist["currentIndex"].as_i64().ok_or("no currentIndex")?;
            let entries = hist["entries"].as_array().cloned().unwrap_or_default();
            let next = idx + params["delta"].as_i64().unwrap_or(0);
            if next < 0 || next as usize >= entries.len() {
                return Err("history step out of range".into());
            }
            let entry = &entries[next as usize];
            self.rt
                .page_command(inst.port as u16, &page.target_id, "Page.navigateToHistoryEntry",
                    json!({"entryId": entry["id"]}))?;
            json!({"ok": true})
        } else {
            self.rt
                .page_command(inst.port as u16, &page.target_id, method, params)?
                .map(|_| json!({"ok": true}))
                .ok_or_else(|| format!("page {} not live", page.target_id))?
        };
        Ok(answer)
    }

    /// POST /page_action {op, url, ctx?} — the p-menu actions on the page
    /// selected by exact URL (mudra.py cmd_page port). close: the
    /// existing /close_page path; move-here: focus the page window then
    /// move it to the ACTIVE workspace; swap: move the page window to the
    /// focused window's workspace and the focused window onto the page's.
    /// niri's move-window-to-workspace acts on the focused window, so
    /// every step focuses first (Python sequence, window resolution from
    /// one snapshot).
    pub fn page_action(&mut self, body: &Value) -> Result<Value, String> {
        let op = body.get("op").and_then(Value::as_str).unwrap_or_default();
        let url = body.get("url").and_then(Value::as_str).unwrap_or_default();
        let ctx = body.get("ctx").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| self.current_ctx());
        if op == "close" {
            return self.close_page(&json!({"ctx": ctx, "query": url}));
        }
        let (ik, inst) = self.alive_instance_for(&ctx).ok_or(format!("ctx {ctx:?} not running"))?;
        let page = self
            .store
            .pages_of_instance(ik.id, false)
            .into_iter()
            .find(|(_, p)| p.closed_at == 0 && p.url == url)
            .map(|(_, p)| p)
            .ok_or(format!("no open page matching {url:?} in {ctx:?}"))?;
        let snap = self.rt.niri_snapshot()?;
        let domain = mudra_store::url_site(&page.url);
        let wid = snap
            .windows
            .iter()
            .filter(|w| w["pid"].as_u64() == Some(inst.pid as u64))
            .find(|w| {
                let t = w["title"].as_str().unwrap_or_default();
                (!page.title.is_empty() && t == page.title) || (!domain.is_empty() && t.contains(&domain))
            })
            .and_then(|w| w["id"].as_u64())
            .ok_or("no niri window matches that page (page not focused into its own window?)")?;
        match op {
            "move-here" => {
                let target = snap.active_workspace_idx().ok_or("no focused workspace")?.to_string();
                self.rt.niri_action(&["focus-window", "--id", &wid.to_string()])?;
                self.rt.niri_action(&["move-window-to-workspace", &target])?;
                Ok(json!({"moved": url, "workspace": target}))
            }
            "swap" => {
                let fwin = snap.focused_window().and_then(|w| w["id"].as_u64());
                let src = fwin
                    .and_then(|f| snap.workspace_idx_of_window(f))
                    .or_else(|| snap.active_workspace_idx())
                    .ok_or("no focused workspace")?
                    .to_string();
                let psrc = snap.workspace_idx_of_window(wid);
                self.rt.niri_action(&["focus-window", "--id", &wid.to_string()])?;
                self.rt.niri_action(&["move-window-to-workspace", &src])?;
                if let (Some(f), Some(p)) = (fwin, psrc)
                    && f != wid {
                    self.rt.niri_action(&["focus-window", "--id", &f.to_string()])?;
                    self.rt.niri_action(&["move-window-to-workspace", &p.to_string()])?;
                }
                Ok(json!({"swapped": url}))
            }
            other => Err(format!("unknown page_action op {other:?}")),
        }
    }

    /// POST /move {ctx, workspace} — every window of the ctx instance to
    /// a workspace (cmd_move port: focus each then move; niri move acts
    /// on the focused window).
    pub fn move_ctx(&mut self, body: &Value) -> Result<Value, String> {
        let ctx = body.get("ctx").and_then(Value::as_str).unwrap_or_default().to_string();
        let workspace = body.get("workspace").and_then(Value::as_str).unwrap_or_default().to_string();
        if workspace.is_empty() {
            return Err("need workspace".into());
        }
        let (_, inst) = self.alive_instance_for(&ctx).ok_or(format!("ctx {ctx:?} not running"))?;
        let snap = self.rt.niri_snapshot()?;
        let wids: Vec<u64> = snap
            .windows
            .iter()
            .filter(|w| w["pid"].as_u64() == Some(inst.pid as u64))
            .filter_map(|w| w["id"].as_u64())
            .collect();
        if wids.is_empty() {
            return Err(format!("no niri windows found for ctx {ctx:?}"));
        }
        for wid in &wids {
            self.rt.niri_action(&["focus-window", "--id", &wid.to_string()])?;
            self.rt.niri_action(&["move-window-to-workspace", &workspace])?;
        }
        Ok(json!({"moved": wids.len(), "ctx": ctx, "workspace": workspace}))
    }

    /// POST /col {action, site?} — column-width memory (cmd_col port).
    /// remember: focused niri window -> instance row -> matching live CDP
    /// page -> tile/output ratio snapped to a band -> SiteWidth row.
    /// show: remembered widths (site-ordered), optional substring filter.
    pub fn col(&mut self, body: &Value) -> Result<Value, String> {
        let action = body.get("action").and_then(Value::as_str).unwrap_or("show");
        if action == "show" {
            let needle = body.get("site").and_then(Value::as_str).unwrap_or_default();
            let rows: Vec<Value> = self
                .store
                .site_widths_all()
                .into_iter()
                .filter(|r| needle.is_empty() || r.site.contains(needle))
                .map(|r| {
                    let (band, frac) = snap_column_width(r.proportion.0);
                    let _ = band;
                    json!({"site": r.site, "proportion": r.proportion.0, "band": frac})
                })
                .collect();
            return Ok(json!({"widths": rows}));
        }
        if action != "remember" {
            return Err(format!("unknown col action {action:?}"));
        }
        let snap = self.rt.niri_snapshot()?;
        let win = snap.focused_window().ok_or("no focused window")?;
        let pid = win["pid"].as_u64().ok_or("window has no pid")? as u32;
        let inst = self
            .store
            .instances
            .scan_keys()
            .into_iter()
            .filter_map(|k| self.store.instances.get(&k).filter(|i| i.running == 1 && i.pid == pid).map(|i| (k, i)))
            .max_by_key(|(k, _)| k.id)
            .ok_or("focused window is not a running mudra instance")?;
        let title = win["title"].as_str().unwrap_or_default();
        let page = self
            .rt
            .list_targets(inst.1.port as u16)?
            .into_iter()
            .find(|t| t["title"].as_str() == Some(title))
            .ok_or_else(|| format!("no CDP page matching focused window title {title:?}"))?;
        let url = page["url"].as_str().unwrap_or_default();
        let domain = mudra_store::url_site(url);
        if domain.is_empty() {
            return Err(format!("cannot derive domain from url {url:?}"));
        }
        let tile = win["layout"]["tile_size"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(serde_json::Value::as_f64)
            .ok_or("window has no layout.tile_size")?;
        let prop = tile / snap.output_width;
        let (band, frac) = snap_column_width(prop);
        let epoch = self.store.site_width_put(&domain, band);
        if epoch.is_some() {
            self.rt.notify(self.store.epoch());
        }
        // `measured` = the raw ratio: `mudra col remember` prints both
        // ("remembered site: 0.498 -> 1/2"), the row keeps the band
        Ok(json!({"site": domain, "measured": prop, "proportion": band, "band": frac}))
    }

    /// POST /conf {ctx, proxy?, extensions?} — per-instance proxy/ext
    /// persistence (cmd_conf port). Omitted fields stay untouched;
    /// "" clears (the CLI spells 'none'/'default' as "").
    pub fn conf(&mut self, body: &Value) -> Result<Value, String> {
        let ctx = body.get("ctx").and_then(Value::as_str).unwrap_or_default();
        if ctx.is_empty() {
            return Err("need ctx".into());
        }
        // situation-leaf check mirrors Python: refuse a non-leaf ctx
        let leaf = self
            .store
            .tag_by_name("situation")
            .into_iter()
            .filter(|(_, t)| t.parent_id == -1)
            .flat_map(|(k, _)| self.store.tag_children(k.id as i32))
            .any(|(_, t)| t.name == ctx);
        if !leaf {
            return Err(format!("not a situation leaf: {ctx:?}"));
        }
        let proxy = body.get("proxy").and_then(Value::as_str);
        let extensions = body.get("extensions").and_then(Value::as_str);
        let k = self.store.config_row(ctx);
        self.store.instance_set_config(&k, proxy, extensions);
        let row = self.store.instances.get(&k).ok_or("row vanished")?;
        Ok(json!({
            "ctx": ctx,
            "proxy": or_none(&row.proxy).map(Value::from).unwrap_or(Value::Null),
            "extensions": or_none(&row.extensions).map(Value::from).unwrap_or(Value::Null),
        }))
    }

    /// POST /dev {on?} — extension dev mode switch (cmd_dev port). Omitted
    /// `on` = read. The seam flag follows the State slot so the very next
    /// spawn sees it (no daemon restart).
    pub fn dev(&mut self, body: &Value) -> Result<Value, String> {
        match body.get("on") {
            Some(v) => {
                let on = v.as_bool().unwrap_or_else(|| v.as_str().is_some_and(|s| matches!(s, "1" | "on" | "true")));
                self.store.put_state_text(mudra_store::state::DEV_MODE, if on { "1" } else { "0" });
                self.rt.set_dev_mode(on);
                Ok(json!({"dev": on}))
            }
            None => Ok(json!({"dev": self.store.state_text(mudra_store::state::DEV_MODE) == "1"})),
        }
    }

    /// POST /tag_seed {} — the initial forest, idempotent (mudra tag init).
    pub fn tag_seed(&mut self) -> Result<Value, String> {
        let n = self.store.seed_tags();
        if n > 0 {
            self.rt.notify(self.store.bump_epoch());
        }
        Ok(json!({"seeded": n}))
    }

    /// POST /tag_set {tag_id, on, page_id?} — single-edge assign/remove
    /// (mudra tag add/remove port; the panel's /set_tags replaces the
    /// whole set, this nudges one edge). Omitted page_id = the FOCUSED
    /// page (_focused_page port: focused niri window -> instance by pid
    /// -> live CDP page by title -> row by target). Idempotent links stay
    /// silent on the epoch but answer honestly.
    pub fn tag_set(&mut self, body: &Value) -> Result<Value, String> {
        let tag_id = body.get("tag_id").and_then(Value::as_u64).unwrap_or(0) as u32;
        let on = body.get("on").and_then(Value::as_bool).unwrap_or(true);
        let tk = mudra_store::TagKey { id: tag_id };
        let tag = self
            .store
            .tags
            .get(&tk)
            .filter(|t| t.deleted == 0)
            .ok_or(format!("tag {tag_id} not found"))?;
        let (pk, page) = match body.get("page_id").and_then(Value::as_u64) {
            Some(id) => {
                let pk = mudra_store::PageKey { id };
                let p = self
                    .store
                    .pages
                    .get(&pk)
                    .filter(|p| p.deleted_at == 0)
                    .ok_or(format!("page {id} not found"))?;
                (pk, p)
            }
            None => {
                let snap = self.rt.niri_snapshot()?;
                let win = snap.focused_window().ok_or("no focused window")?;
                let pid = win["pid"].as_u64().ok_or("window has no pid")? as u32;
                let title = win["title"].as_str().unwrap_or_default().to_string();
                let inst = self
                    .store
                    .instances
                    .scan_keys()
                    .into_iter()
                    .filter_map(|k| {
                        self.store
                            .instances
                            .get(&k)
                            .filter(|i| i.running == 1 && i.pid == pid)
                            .map(|i| (k, i))
                    })
                    .max_by_key(|(k, _)| k.id)
                    .ok_or("focused window is not a running mudra instance")?;
                let target = self
                    .rt
                    .list_targets(inst.1.port as u16)?
                    .into_iter()
                    .find(|t| !title.is_empty() && t["title"].as_str() == Some(title.as_str()))
                    .ok_or_else(|| format!("no CDP page matching focused window title {title:?}"))?;
                let tid = target["id"].as_str().unwrap_or_default();
                self.store
                    .page_by_target(tid)
                    .ok_or_else(|| "focused page not recorded (sync pending?)".to_string())?
            }
        };
        let current = self.store.tags_of_page(&pk).contains(&tk);
        if on && !current {
            self.store.link_page_tag(&pk, &tk);
            self.rt.notify(self.store.bump_epoch());
        } else if !on && current {
            self.store.unlink_page_tag(&pk, &tk);
            self.rt.notify(self.store.bump_epoch());
        }
        let label = if page.title.is_empty() { page.url.clone() } else { page.title.clone() };
        Ok(json!({
            "page_id": pk.id, "tag_id": tag_id, "tag": tag.name,
            "label": label, "on": on, "changed": on != current,
        }))
    }

    /// POST /sort {kind} — the sort preference slot (mudra sort port;
    /// the panel keeps its own in-memory toggle, this is the CLI's).
    pub fn sort(&mut self, body: &Value) -> Result<Value, String> {
        let kind = body.get("kind").and_then(Value::as_str).unwrap_or_default();
        if !matches!(kind, "mru" | "mtime" | "rating") {
            return Err(format!("unknown sort kind {kind:?}"));
        }
        self.store.put_state_text(mudra_store::state::SORT, kind);
        Ok(json!({"sort": kind}))
    }

    /// POST /panel {action} — the console window lifecycle (ui.launch
    /// port; the orchestration lives backend-side so the CLI stays a
    /// pure forwarder). focus: niri-focus the existing panel window when
    /// one is up (identified by process cmdline, never title — the
    /// Python rule), spawn a fresh one otherwise. spawn: force a new
    /// window. status: is a panel window up right now (live cmdline
    /// probe, not the last-known pid).
    pub fn panel(&mut self, body: &Value) -> Result<Value, String> {
        match body.get("action").and_then(Value::as_str).unwrap_or("focus") {
            "status" => {
                let snap = self.rt.niri_snapshot()?;
                let up = snap
                    .windows
                    .iter()
                    .any(|w| w["pid"].as_u64().is_some_and(|p| crate::spawn::cmdline_has(p, "panel-profile")));
                Ok(json!({"running": up}))
            }
            "spawn" => {
                let pid = self.rt.launch_panel()?;
                self.store.put_state_text(mudra_store::state::PANEL_PID, &pid.to_string());
                Ok(json!({"spawned": pid}))
            }
            "focus" => {
                let snap = self.rt.niri_snapshot()?;
                let wid = snap
                    .windows
                    .iter()
                    .filter(|w| w["pid"].as_u64().is_some_and(|p| crate::spawn::cmdline_has(p, "panel-profile")))
                    .filter_map(|w| w["id"].as_u64())
                    .next();
                match wid {
                    Some(w) => {
                        self.rt.niri_action(&["focus-window", "--id", &w.to_string()])?;
                        Ok(json!({"focused": w}))
                    }
                    // ui.launch parity: no window up -> one spawn starts
                    // fresh (pid persisted for diagnostics only)
                    None => {
                        let pid = self.rt.launch_panel()?;
                        self.store.put_state_text(mudra_store::state::PANEL_PID, &pid.to_string());
                        Ok(json!({"spawned": pid}))
                    }
                }
            }
            other => Err(format!("unknown panel action {other:?}")),
        }
    }
}

/// Column-width snap bands (niri preset-column-widths gradient; the
/// Python wm.SNAP_BANDS port). Returns (ratio band, human fraction).
pub fn snap_column_width(ratio: f64) -> (f64, &'static str) {
    const BANDS: [(f64, &str); 4] = [(1.0 / 3.0, "1/3"), (0.5, "1/2"), (2.0 / 3.0, "2/3"), (1.0, "1")];
    BANDS
        .into_iter()
        .min_by(|a, b| (a.0 - ratio).abs().total_cmp(&(b.0 - ratio).abs()))
        .unwrap()
}

fn or_none(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

/// tabId as text: string or number both spell it (Python str(tab_id)).
fn tab_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}
