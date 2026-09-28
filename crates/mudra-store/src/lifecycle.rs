//! Page/instance lifecycle transitions — the port of `mudralib/db.py`'s
//! page write paths and `mudrad.py`'s `_sync_infos` / `_close_target` /
//! `_mark_down` teardown (behavior spec: the Python tree until R2).
//!
//! One law binds every function here: a transition that changes stored
//! state bumps the epoch and returns it, so the caller's invalidation
//! push cannot drift from the write. No-ops return `None` and stay silent
//! — the panel must not re-scan for writes that wrote nothing.
//!
//! Timestamps are caller-supplied millis (the daemon owns the clock;
//! purity here keeps the index funcs safe and tests deterministic).
//! `0` is the absent sentinel for `opened_at`/`closed_at`/`deleted_at`,
//! `0` is "no parent" for `parent_id` (page ids start at 1).

use crate::*;

/// One CDP targetInfo snapshot (type `page` only — the daemon filters
/// before handing batches here, same as `_sync_infos`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetInfo {
    pub target_id: String,
    pub url: String,
    pub title: String,
    /// CDP `openerId`: the target that opened this one ("" = none).
    pub opener_id: String,
}

/// Why an upsert landed where it did (log/diagnostics; tests observe the
/// row state instead).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpsertMode {
    /// Same (instance, target) row exists — refresh url/title, reopen.
    Refresh,
    /// Window reopened with a new target id: took over the latest closed
    /// same-URL row (rebound target, cleared closed_at, fresh opened_at).
    Revive,
    /// Genuinely new page: new id, position = per-instance max + 1.
    Insert,
}

impl MudraStore {
    /// The instance row owning a context (latest by id — the Python
    /// `ORDER BY id DESC LIMIT 1` reuse rule that carries proxy/extensions
    /// across restarts).
    pub fn instance_for_context(&self, ctx: &str) -> Option<(InstanceKey, Instance)> {
        self.instance_by_profile(ctx).into_iter().max_by_key(|(k, _)| k.id)
    }

    /// Record a launch: alive-reuse updates port/pid/running on the old
    /// row (proxy/extensions carry over untouched); no old row creates
    /// one. Port of `instance_launch_started`.
    pub fn launch_started(
        &mut self,
        reuse: Option<&InstanceKey>,
        profile: &str,
        port: u32,
        pid: u32,
        proxy: Option<&str>,
        extensions: Option<&str>,
    ) -> InstanceKey {
        if let Some(k) = reuse {
            let mut row = self.instances.get(k).expect("reused row must exist");
            row.port = port;
            row.pid = pid;
            row.running = 1;
            self.instances.put(k, &row);
            return k.clone();
        }
        let k = InstanceKey { id: self.next_id(state::INSTANCE_ID) as u32 };
        self.instances.put(
            &k,
            &Instance {
                profile: profile.to_string(),
                port,
                pid,
                running: 1,
                proxy: proxy.unwrap_or_default().to_string(),
                extensions: extensions.unwrap_or_default().to_string(),
            },
        );
        k
    }

    /// Mark an instance down (the unified teardown when its watcher
    /// ends): running=0 and every still-open page of the instance gets
    /// `closed_at = ts`. Port of `_mark_down` (set_running + pages_close_all).
    pub fn mark_down(&mut self, instance_id: u32, ts: u64) -> Option<u64> {
        let mut changed = false;
        let k = InstanceKey { id: instance_id };
        if let Some(mut inst) = self.instances.get(&k)
            && inst.running != 0
        {
            inst.running = 0;
            self.instances.put(&k, &inst);
            changed = true;
        }
        for (pk, mut p) in self.pages_of_instance(instance_id, true) {
            if p.closed_at == 0 {
                p.closed_at = ts;
                self.pages.put(&pk, &p);
                changed = true;
            }
        }
        changed.then(|| self.bump_epoch())
    }

    /// Sync one batch of CDP targetInfos into the pages table — the port
    /// of `_sync_infos`: upsert each page-type target, then backfill the
    /// openerId parent links (batch-local id map first, DB lookup for an
    /// opener outside the batch). One epoch bump per batch.
    pub fn sync_targets(
        &mut self,
        instance_id: u32,
        infos: &[TargetInfo],
        ts: u64,
    ) -> Option<u64> {
        if infos.is_empty() {
            return None;
        }
        let mut t2page: std::collections::HashMap<&str, PageKey> =
            std::collections::HashMap::new();
        for info in infos {
            let (key, _) = self.upsert_target(instance_id, info, ts);
            t2page.insert(info.target_id.as_str(), key);
        }
        // parent backfill: child's parent_id = the page that opened it,
        // set only on first sight (never overwrites) — page_set_parent_once.
        for info in infos {
            if info.opener_id.is_empty() {
                continue;
            }
            let Some(child) = t2page.get(info.target_id.as_str()) else {
                continue;
            };
            let parent = match t2page.get(info.opener_id.as_str()) {
                Some(p) => Some(p.clone()),
                None => self.page_by_target(&info.opener_id).map(|(k, _)| k),
            };
            if let Some(parent) = parent {
                self.set_parent_once(child, &parent);
            }
        }
        Some(self.bump_epoch())
    }

    /// One CDP targetInfo -> pages row (the port of `page_upsert_by_target`).
    /// Identity order: (instance, target) row wins; else revive the latest
    /// closed same-URL row; else insert. Returns the row key and why.
    pub fn upsert_target(
        &mut self,
        instance_id: u32,
        info: &TargetInfo,
        ts: u64,
    ) -> (PageKey, UpsertMode) {
        // (a) same instance + same target: refresh url/title, reopen.
        if let Some((k, mut row)) = self
            .pages_of_instance(instance_id, true)
            .into_iter()
            .find(|(_, p)| p.target_id == info.target_id)
        {
            row.url = info.url.clone();
            row.title = info.title.clone();
            row.closed_at = 0;
            self.pages.put(&k, &row);
            return (k, UpsertMode::Refresh);
        }
        // (b) reopen lands a NEW target id: take over the latest closed,
        // live row of the same instance + URL (the E2E contract the Python
        // tree fixed: close -> reopen keeps the same page id).
        if let Some((k, mut row)) = self
            .pages_of_instance(instance_id, false)
            .into_iter()
            .filter(|(_, p)| p.closed_at != 0 && p.url == info.url)
            .max_by_key(|(_, p)| p.closed_at)
        {
            row.target_id = info.target_id.clone();
            row.title = info.title.clone();
            row.closed_at = 0;
            row.opened_at = ts;
            self.pages.put(&k, &row);
            return (k, UpsertMode::Revive);
        }
        // (c) genuinely new: position = per-instance max + 1.
        let position = self
            .pages_of_instance(instance_id, true)
            .into_iter()
            .map(|(_, p)| p.position)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0);
        let k = PageKey { id: self.next_id(state::PAGE_ID) };
        self.pages.put(
            &k,
            &Page {
                instance_id,
                target_id: info.target_id.clone(),
                url: info.url.clone(),
                title: info.title.clone(),
                position,
                opened_at: ts,
                ..Default::default()
            },
        );
        (k, UpsertMode::Insert)
    }

    /// Mark a target closed (the single teardown path: an active close
    /// never DELETEs — the row survives with `closed_at` set). Port of
    /// `page_close_target` (UPDATE ... WHERE closed_at IS NULL).
    pub fn close_target(&mut self, instance_id: u32, target_id: &str, ts: u64) -> Option<u64> {
        let found = self
            .pages_of_instance(instance_id, false)
            .into_iter()
            .find(|(_, p)| p.target_id == target_id && p.closed_at == 0);
        match found {
            Some((k, mut p)) => {
                p.closed_at = ts;
                self.pages.put(&k, &p);
                Some(self.bump_epoch())
            }
            None => None,
        }
    }

    /// Backfill the openerId parent link, set-only-once (an existing
    /// parent never moves; manual values are not overwritten). Port of
    /// `page_set_parent_once` (WHERE parent_id IS NULL).
    pub fn set_parent_once(&mut self, child: &PageKey, parent: &PageKey) -> bool {
        match self.pages.get(child) {
            Some(mut p) if p.parent_id == 0 => {
                p.parent_id = parent.id;
                self.pages.put(child, &p);
                true
            }
            _ => false,
        }
    }

    /// Soft delete (sets deleted_at): allowed only for closed pages — the
    /// invariant lives in the store because every caller must not
    /// re-implement it (ops.py's "cannot delete an open page" error).
    /// Success always writes, so the new epoch is returned directly.
    pub fn delete_page(&mut self, page: &PageKey, ts: u64) -> Result<u64, String> {
        match self.pages.get(page) {
            None => Err(format!("page {} not found", page.id)),
            Some(p) if p.deleted_at != 0 => Err("page is deleted".into()),
            Some(p) if p.closed_at == 0 => {
                Err("cannot delete an open page; close it first".into())
            }
            Some(mut p) => {
                p.deleted_at = ts;
                self.pages.put(page, &p);
                Ok(self.bump_epoch())
            }
        }
    }

    /// Toggle a tag on a page (junction link/unlink — port of
    /// `page_tag_toggle` returning "added"/"removed").
    pub fn page_tag_toggle(&mut self, page: &PageKey, tag: &TagKey) -> bool {
        let linked = self.tags_of_page(page).contains(tag);
        if linked {
            self.unlink_page_tag(page, tag);
        } else {
            self.link_page_tag(page, tag);
        }
        // The tree/tag display reads derive from junction entries, so a
        // toggle is a page-data invalidation too.
        self.bump_epoch();
        !linked
    }

    /// Replace a page's whole tag set (the panel's batch assignment —
    /// port of ui.py `_set_tags`): delete all junction links, re-link the
    /// given ids after dropping ids that do not resolve to a live tag
    /// (deleted tags filter out exactly like the Python `deleted=0`
    /// guard). Always bumps: a replace is a write even when the set ends
    /// up equal, keeping the invalidation contract one-write-one-bump.
    pub fn page_tag_replace(&mut self, page: &PageKey, tag_ids: &[u32]) -> u64 {
        let keep: Vec<TagKey> = tag_ids
            .iter()
            .map(|id| TagKey { id: *id })
            .filter(|tk| self.tags.get(tk).is_some_and(|t| t.deleted == 0))
            .collect();
        for old in self.tags_of_page(page) {
            self.unlink_page_tag(page, &old);
        }
        for tk in keep {
            self.link_page_tag(page, &tk);
        }
        self.bump_epoch()
    }

    /// Create a tag node under `parent_id` (-1 = root), idempotent on
    /// (parent, name) among live rows — port of ui.py `_create_tag`:
    /// a duplicate returns the existing id without writing. Success bumps
    /// the epoch (the forest column re-renders); `name` is assumed
    /// already trimmed and non-empty (the verb layer enforces it).
    pub fn create_tag(&mut self, parent_id: i32, name: &str) -> (TagKey, bool) {
        if let Some((k, _)) = self
            .tag_children(parent_id)
            .into_iter()
            .find(|(_, t)| t.name == name)
        {
            return (k, false);
        }
        let k = TagKey { id: self.next_id(state::TAG_ID) as u32 };
        self.tags.put(
            &k,
            &Tag {
                parent_id,
                name: name.to_string(),
                ..Default::default()
            },
        );
        self.bump_epoch();
        (k, true)
    }

    /// Close a page by row id (the panel's `close` op, port of
    /// ops.close_page): set closed_at AND ask the machine to close the
    /// target. The watcher's destroyed event may also arrive and re-mark
    /// — close_target's `closed_at == 0` guard keeps that idempotent.
    /// Returns the new epoch, or None when the row is not open (no write).
    pub fn close_page(&mut self, page: &PageKey, ts: u64) -> Option<(String, u64)> {
        let p = self.pages.get(page)?;
        if p.closed_at != 0 {
            return None;
        }
        let mut p = p;
        p.closed_at = ts;
        self.pages.put(page, &p);
        Some((p.target_id, self.bump_epoch()))
    }

    /// Switch the current context: valid only for a leaf of the
    /// `situation` tree (the port of db.set_context's subquery check).
    /// Returns the bumped epoch, or None when rejected (no write).
    pub fn set_context(&mut self, name: &str) -> Option<u64> {
        let under_situation = self.tag_by_name(name).into_iter().any(|(_, t)| {
            t.deleted == 0
                && self
                    .tags
                    .get(&TagKey { id: t.parent_id.max(0) as u32 })
                    .filter(|p| p.name == "situation" && p.parent_id == -1)
                    .is_some()
        });
        if !under_situation {
            return None;
        }
        self.put_state_text(state::CURRENT_CONTEXT, name);
        Some(self.bump_epoch())
    }

    /// Full path of a tag ("state::unread"; root nodes return their bare
    /// name; a broken parent chain stops at the last resolvable node).
    /// Port of `page_tag_paths`' parent walk — the capsule segment source.
    pub fn tag_path(&self, tag: &TagKey) -> Vec<String> {
        let mut parts = Vec::new();
        let mut cur = self.tags.get(tag);
        let mut seen_guard = 0usize;
        while let Some(t) = cur {
            parts.push(t.name.clone());
            if t.parent_id == -1 || seen_guard > 64 {
                break;
            }
            cur = self.tags.get(&TagKey { id: t.parent_id as u32 });
            seen_guard += 1;
        }
        parts.reverse();
        parts
    }

    /// The `::`-joined display path (capsule rendering contract).
    pub fn tag_path_string(&self, tag: &TagKey) -> String {
        self.tag_path(tag).join("::")
    }
}
