//! The panel's control-plane bridge: mudrad's verbs on :8899.
//!
//! Division of labor (R2 方案 A): the WS frame channel is the read-only
//! KV data plane plus epoch hints; every write and every server-shaped
//! read (/forest, /ctx_pages, /shot) rides plain HTTP POST to the single
//! control point — a frame PUT would bypass the Collection's index and
//! epoch maintenance, and a bespoke WS op channel was retired once and
//! must not be revived (PLAN §11).
//!
//! The daemon answers every response with `Access-Control-Allow-Origin: *`
//! (same as the old intercept server), so a panel page on :9299 posting
//! to :8899 needs no proxy.

use serde::de::DeserializeOwned;
use serde_json::Value;

/// mudrad `CONTROL_PORT` — the one control point (the same constant the
/// extension's SW bridge uses; hardcoded like Python's, by design).
pub const CONTROL_PORT: u16 = 8899;

fn url(path: &str) -> String {
    format!("http://127.0.0.1:{CONTROL_PORT}{path}")
}

async fn call_json<T: DeserializeOwned>(path: &str, body: Value) -> Result<T, String> {
    let resp = gloo_net::http::Request::post(&url(path))
        .json(&body)
        .map_err(|e| format!("{path}: encode: {e}"))?
        .send()
        .await
        .map_err(|e| format!("{path}: {e}"))?;
    if !resp.ok() {
        // the daemon's error branch carries {ok:false, err:"..."} — pass
        // it through verbatim (the error passthrough rule)
        let text = resp.text().await.unwrap_or_default();
        let err = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("err").and_then(Value::as_str).map(str::to_string))
            .unwrap_or(text);
        return Err(format!("{path}: {err}"));
    }
    resp.json::<T>().await.map_err(|e| format!("{path}: decode: {e}"))
}

/// One verb, discarding the response payload (writes: the epoch hint
/// arriving on the WS triggers the re-scan).
pub async fn verb(path: &str, body: Value) -> Result<(), String> {
    call_json::<Value>(path, body).await.map(|_| ())
}

/// GET /config — the flat two-layer-merged dict. The panel reads exactly two
/// keys today (`ui.thumbnails`, `ui.indent`); a failure reads as the defaults
/// (Python parity: a missing/broken config never breaks the view).
#[derive(Clone, Copy)]
pub struct Ui {
    /// hover screenshot preview on page links
    pub thumbnails: bool,
    /// one nesting level of the page tree, in em
    pub indent: f64,
}

impl Default for Ui {
    fn default() -> Self {
        Ui { thumbnails: false, indent: 1.4 }
    }
}

pub async fn ui() -> Ui {
    #[derive(serde::Deserialize)]
    struct Cfg {
        #[serde(default)]
        thumbnails: bool,
        #[serde(default = "default_indent")]
        indent: f64,
    }
    fn default_indent() -> f64 {
        Ui::default().indent
    }
    #[derive(serde::Deserialize)]
    struct Wrap {
        config: Cfg,
    }
    let Ok(resp) = gloo_net::http::Request::get(&url("/config")).send().await else {
        return Ui::default();
    };
    if !resp.ok() {
        return Ui::default();
    }
    resp.json::<Wrap>()
        .await
        .map(|w| Ui { thumbnails: w.config.thumbnails, indent: w.config.indent })
        .unwrap_or_default()
}

// ---- typed payloads for the shaped reads ----

/// One tag-forest node (the /forest shape). `root` carries no `path`;
/// `rank_axis` only appears on roots; `rank` is null for plain tags
/// (mudrad marshals the row's `0` sentinel as JSON null — the panel
/// discriminates plain vs rank on exactly that, like the JS original).
#[derive(serde::Deserialize, Clone, Debug, PartialEq)]
pub struct TagNode {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub rank: Option<i32>,
    #[serde(default)]
    pub root: bool,
    #[serde(default)]
    pub rank_axis: Option<String>,
    #[serde(default)]
    pub children: Vec<TagNode>,
}

/// One /forest response (forest + contexts + current in one round trip).
#[derive(serde::Deserialize, Clone, Debug)]
pub struct Forest {
    pub forest: Vec<TagNode>,
    pub contexts: Vec<String>,
    pub current: String,
}

/// One /ctx_pages row (open AND closed-undeleted pages; `closed` drives
/// the strikethrough and the ↻/🗑 affordances).
#[derive(serde::Deserialize, Clone, Debug, PartialEq)]
pub struct PageInfo {
    pub id: u64,
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub position: u32,
    #[serde(default)]
    pub tag_ids: Vec<u32>,
    #[serde(default)]
    pub target_id: String,
    #[serde(default)]
    pub parent_id: u64,
    #[serde(default)]
    pub opened_at: u64,
    #[serde(default)]
    pub closed: bool,
}

#[derive(serde::Deserialize, Clone, Debug)]
pub struct Pages {
    pub pages: Vec<PageInfo>,
}

pub async fn forest() -> Result<Forest, String> {
    call_json("/forest", Value::Null).await
}

pub async fn ctx_pages(ctx: &str) -> Result<Pages, String> {
    call_json("/ctx_pages", serde_json::json!({ "ctx": ctx })).await
}

#[derive(serde::Deserialize)]
struct ShotResp {
    data: Option<String>,
}

pub async fn shot(page_id: u64) -> Result<Option<String>, String> {
    Ok(call_json::<ShotResp>("/shot", serde_json::json!({ "page_id": page_id }))
        .await?
        .data)
}

#[derive(serde::Deserialize)]
struct TagId {
    id: u32,
}

pub async fn create_tag(parent_id: u32, name: &str) -> Result<u32, String> {
    Ok(call_json::<TagId>(
        "/create_tag",
        serde_json::json!({ "parent_id": parent_id, "name": name }),
    )
    .await?
    .id)
}

// ---- the two operator surfaces the CLI used to own alone -------------
// (`/events` = ns 8's replay window, `/hook_list` = ns 9's rule set)

/// One event row: `args` is the self-describing snapshot the emit site
/// recorded — an object for every lifecycle kind, a bare string when a
/// producer wrote one — so it rides as a `Value`.
#[derive(serde::Deserialize, Clone, Debug, PartialEq)]
pub struct EventRow {
    pub id: u64,
    pub kind: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub at: u64,
}

#[derive(serde::Deserialize, Clone, Debug)]
pub struct Events {
    pub events: Vec<EventRow>,
    /// The log head id. Informational: the panel's log view reads by
    /// `tail`, never by cursor.
    #[serde(default)]
    pub head: u64,
}

/// The newest `tail` rows — the daemon anchors them server-side (the
/// panel keeps no cursor for a log view; a re-pull is an overwrite).
pub async fn events(tail: u64) -> Result<Events, String> {
    call_json("/events", serde_json::json!({ "tail": tail })).await
}

/// One hook row. The code rides along in full; the view decides how
/// much of it to show.
#[derive(serde::Deserialize, Clone, Debug, PartialEq)]
pub struct HookRow {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub matches: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub enabled: u8,
    #[serde(default)]
    pub updated_at: u64,
}

#[derive(serde::Deserialize, Clone, Debug)]
pub struct Hooks {
    pub hooks: Vec<HookRow>,
}

pub async fn hook_list() -> Result<Hooks, String> {
    call_json("/hook_list", serde_json::json!({})).await
}
