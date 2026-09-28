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
