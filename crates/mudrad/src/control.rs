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
            return Ok(json!({"ctx": Value::Null, "tags": [], "role": role}));
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
        Ok(json!({"ctx": ctx, "tags": tags, "role": role}))
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
