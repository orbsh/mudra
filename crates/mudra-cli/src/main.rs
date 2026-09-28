//! `mudra` — the thin CLI (Rust rewrite of mudra.py). Iron rule: the CLI
//! only parses args, POSTs to mudrad's :8899 control point, and prints.
//! Zero DB access, zero process operations: every state transition runs
//! backend-side (the single-control-point decision).
//!
//! Output wording mirrors the Python CLI exactly (`repr`-style quotes,
//! column widths, error lines) so muscle memory and scripts survive the
//! port. Verbs retired with the launcher (`menu`) are gone; everything
//! else has a 1:1 command.

use std::process::exit;

use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{json, Value};

const CONTROL: &str = "http://127.0.0.1:8899";

/// Python f"{s!r}" for plain strings: single quotes.
fn repr(s: &str) -> String {
    format!("'{s}'")
}

/// One POST to the control point: daemon-down and backend-error are both
/// loud exits (Python's `_ctl` SystemExit parity).
fn ctl(path: &str, body: Value) -> Value {
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mudra: client: {e}");
            exit(1);
        }
    };
    let resp = match client.post(format!("{CONTROL}{path}")).json(&body).send() {
        Ok(r) => r,
        Err(_) => {
            eprintln!("mudrad not running (start: mudrad run)");
            exit(1);
        }
    };
    let v: Value = match resp.json() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("mudrad: bad response: {e}");
            exit(1);
        }
    };
    if v["ok"].as_bool() == Some(true) {
        return v;
    }
    let err = v["err"].as_str().unwrap_or("unknown").to_string();
    eprintln!("mudrad: {err}");
    exit(1);
}

/// Same but the error line is the bare message (focus prints errors on
/// stdout like Python's `print(e)` inside the command, not via _ctl).
fn ctl_soft(path: &str, body: Value) -> Result<Value, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .post(format!("{CONTROL}{path}"))
        .json(&body)
        .send()
        .map_err(|_| "mudrad not running (start: mudrad run)".to_string())?;
    let status = resp.status();
    let v: Value = resp.json().map_err(|e| e.to_string())?;
    if status.is_success() && v["ok"].as_bool() == Some(true) {
        return Ok(v);
    }
    Err(v["err"].as_str().unwrap_or("unknown").to_string())
}

#[derive(ValueEnum, Clone)]
enum PageOp {
    MoveHere,
    Swap,
    Close,
}

#[derive(ValueEnum, Clone)]
enum SortKind {
    Mru,
    Mtime,
    Rating,
}

#[derive(Parser)]
#[command(name = "mudra", about = "browser context manager")]
struct Opts {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// list contexts / pages of one ctx
    Ls {
        /// list pages of a context (situation leaf)
        ctx: Option<String>,
        /// filter pages by url/title substring
        #[arg(long, short = 'f')]
        filter: Option<String>,
    },
    /// open url in a context (spawn instance + first url)
    Open {
        url: String,
        /// situation leaf (default: current context)
        #[arg(long)]
        ctx: Option<String>,
    },
    /// list live page targets (CDP)
    Targets { ctx: String },
    /// find page by url/title and activate (ctx optional -> current)
    Focus {
        query: String,
        #[arg(long)]
        ctx: Option<String>,
    },
    /// navigate current page to url
    Goto {
        url: String,
        #[arg(long)]
        ctx: Option<String>,
    },
    /// history back
    Back {
        #[arg(long)]
        ctx: Option<String>,
    },
    /// history forward
    Forward {
        #[arg(long)]
        ctx: Option<String>,
    },
    /// reload current page
    Reload {
        #[arg(long)]
        ctx: Option<String>,
    },
    /// page mode actions: move-here / swap / close on the selected page
    Page {
        op: PageOp,
        url: String,
        #[arg(long)]
        ctx: Option<String>,
    },
    /// move a context's windows to a workspace
    Move {
        ctx: String,
        /// target niri workspace
        workspace: String,
    },
    /// set / show current context (situation leaf)
    Ctx { ctx: Option<String> },
    /// extension dev mode: clear chromium extension caches on spawn
    Dev {
        /// on / off (omit to show current)
        on: Option<String>,
    },
    /// add a page to the running context instance
    Add {
        url: String,
        #[arg(long)]
        ctx: Option<String>,
    },
    /// close a tab (<query>) or a whole context instance
    Close {
        /// url filter -> close just that open tab
        query: Option<String>,
        #[arg(long)]
        ctx: Option<String>,
    },
    /// set per-context proxy/extensions config
    Conf {
        /// situation leaf
        ctx: String,
        /// proxy e.g. 127.0.0.1:7890, or 'none'
        #[arg(long)]
        proxy: Option<String>,
        /// comma-separated extension dirs, or 'default'
        #[arg(long = "ext")]
        ext: Option<String>,
    },
    /// column-width memory: remember|show
    Col {
        /// remember capture focused window width; show list
        #[arg(default_value = "show")]
        action: String,
        /// filter by site in show
        site: Option<String>,
    },
    /// tag forest: init seed / add|remove assignment to pages
    Tag {
        action: String,
        /// tag id (add/remove)
        tag_id: Option<String>,
        /// target page id (omit = currently focused page)
        page_id: Option<String>,
    },
    /// launch the management panel (focus existing window or spawn)
    Ui,
    /// set sort preference (MRU/time/rating)
    Sort { kind: SortKind },
}

fn main() {
    let opts = Opts::parse();
    let code = match opts.cmd {
        Cmd::Ls { ctx, filter } => ls(ctx, filter),
        Cmd::Open { url, ctx } => open(&url, ctx),
        Cmd::Targets { ctx } => targets(&ctx),
        Cmd::Focus { query, ctx } => focus(&query, ctx),
        Cmd::Goto { url, ctx } => nav("goto", Some(url), ctx),
        Cmd::Back { ctx } => nav("back", None, ctx),
        Cmd::Forward { ctx } => nav("forward", None, ctx),
        Cmd::Reload { ctx } => nav("reload", None, ctx),
        Cmd::Page { op, url, ctx } => page(op, &url, ctx),
        Cmd::Move { ctx, workspace } => mv(&ctx, &workspace),
        Cmd::Ctx { ctx } => show_set_ctx(ctx),
        Cmd::Dev { on } => dev(on),
        Cmd::Add { url, ctx } => add(&url, ctx),
        Cmd::Close { query, ctx } => close(query, ctx),
        Cmd::Conf { ctx, proxy, ext } => conf(&ctx, proxy, ext),
        Cmd::Col { action, site } => col(&action, site),
        Cmd::Tag { action, tag_id, page_id } => tag(&action, tag_id, page_id),
        Cmd::Ui => ui(),
        Cmd::Sort { kind } => {
            let kind = match kind {
                SortKind::Mru => "mru",
                SortKind::Mtime => "mtime",
                SortKind::Rating => "rating",
            };
            ctl("/sort", json!({"kind": kind}));
            println!("sort -> {kind}");
            0
        }
    };
    exit(code);
}

fn with_ctx(body: Value, ctx: Option<String>) -> Value {
    match ctx {
        Some(c) => {
            let mut v = body;
            v["ctx"] = json!(c);
            v
        }
        None => body,
    }
}

fn ls(ctx: Option<String>, filter: Option<String>) -> i32 {
    match ctx {
        Some(c) => {
            let r = ctl("/ctx_pages", json!({"ctx": c}));
            let all = r["pages"].as_array().cloned().unwrap_or_default();
            let pages: Vec<&Value> = match &filter {
                Some(f) => all
                    .iter()
                    .filter(|p| {
                        format!("{} {}", p["url"].as_str().unwrap_or_default(), p["title"].as_str().unwrap_or_default())
                            .contains(f.as_str())
                    })
                    .collect(),
                None => all.iter().collect(),
            };
            let open_n = pages.iter().filter(|p| p["closed"].as_bool() != Some(true)).count();
            println!("ctx {}: {open_n} open / {} pages", repr(&c), pages.len());
            for p in pages {
                let mark = if p["closed"].as_bool() == Some(true) { "[closed]" } else { "[open]" };
                println!(
                    "  {mark} #{} {}  {}",
                    p["position"].as_i64().unwrap_or(0),
                    p["url"].as_str().unwrap_or_default(),
                    p["title"].as_str().unwrap_or_default()
                );
            }
        }
        None => {
            let r = ctl("/contexts", json!({}));
            for row in r["contexts"].as_array().cloned().unwrap_or_default() {
                let mark = if row["current"].as_bool() == Some(true) { "*" } else { " " };
                println!(
                    "{mark} {:<20} {} pages",
                    row["leaf"].as_str().unwrap_or_default(),
                    row["pages"].as_u64().unwrap_or(0)
                );
            }
        }
    }
    0
}

fn open(url: &str, ctx: Option<String>) -> i32 {
    let r = ctl("/open", with_ctx(json!({"url": url}), ctx));
    if r["mode"].as_str() == Some("joined") {
        println!(
            "joined running instance of {} (port {})",
            repr(r["ctx"].as_str().unwrap_or_default()),
            r["port"].as_u64().unwrap_or(0)
        );
    } else {
        println!(
            "opened {} in ctx {} (port {}, pid {})",
            repr(url),
            repr(r["ctx"].as_str().unwrap_or_default()),
            r["port"].as_u64().unwrap_or(0),
            r["pid"].as_u64().unwrap_or(0)
        );
    }
    0
}

fn targets(ctx: &str) -> i32 {
    let r = ctl("/targets", json!({"ctx": ctx}));
    for t in r["targets"].as_array().cloned().unwrap_or_default() {
        let tid = t["targetId"].as_str().unwrap_or_default();
        // char-wise truncation (Python [:8]/[:40] counts characters, not
        // bytes — titles carry CJK); manual padding for the 40-col field
        let t8: String = tid.chars().take(8).collect();
        let title = t["title"].as_str().unwrap_or_default();
        let t40: String = title.chars().take(40).collect();
        let pad = 40usize.saturating_sub(t40.chars().count());
        println!(
            "{t8}  {t40}{} {}",
            " ".repeat(pad),
            t["url"].as_str().unwrap_or_default()
        );
    }
    0
}

/// Python's cmd_focus printed the ValueError text on STDOUT and exit 1.
fn focus(query: &str, ctx: Option<String>) -> i32 {
    match ctl_soft("/focus", with_ctx(json!({"query": query}), ctx)) {
        Ok(r) => {
            println!("focused: {}", r["label"].as_str().unwrap_or_default());
            0
        }
        Err(e) => {
            println!("{e}");
            1
        }
    }
}

fn nav(cmd: &str, url: Option<String>, ctx: Option<String>) -> i32 {
    let mut body = json!({"cmd": cmd});
    if let Some(u) = url {
        body["url"] = json!(u);
    }
    if let Some(c) = ctx {
        body["ctx"] = json!(c);
    }
    ctl("/nav", body);
    0
}

fn page(op: PageOp, url: &str, ctx: Option<String>) -> i32 {
    let name = match op {
        PageOp::MoveHere => "move-here",
        PageOp::Swap => "swap",
        PageOp::Close => "close",
    };
    let body = with_ctx(json!({"op": name, "url": url}), ctx);
    match ctl_soft("/page_action", body) {
        Ok(r) => {
            match name {
                "close" => println!("closed page {}", r["closed"].as_str().unwrap_or_default()),
                "move-here" => println!(
                    "moved {} to active workspace",
                    r["moved"].as_str().unwrap_or_default()
                ),
                _ => println!(
                    "swapped {} with focused window",
                    r["swapped"].as_str().unwrap_or_default()
                ),
            }
            0
        }
        Err(e) => {
            println!("{e}");
            1
        }
    }
}

fn mv(ctx: &str, workspace: &str) -> i32 {
    let r = ctl("/move", json!({"ctx": ctx, "workspace": workspace}));
    println!(
        "moved {} window(s) of {} to workspace {workspace}",
        r["moved"].as_u64().unwrap_or(0),
        repr(ctx)
    );
    0
}

fn show_set_ctx(ctx: Option<String>) -> i32 {
    match ctx {
        None => {
            let r = ctl("/ctx_current", json!({}));
            let cur = r["ctx"].as_str().unwrap_or_default();
            println!("current ctx: {}", if cur.is_empty() { "(none)" } else { cur });
            0
        }
        Some(c) => match ctl_soft("/ctx", json!({"ctx": c})) {
            Ok(_) => {
                println!("current ctx -> {c}");
                0
            }
            Err(e) => {
                // Python printed the leaf-check on stdout before the POST
                println!("{e}");
                1
            }
        },
    }
}

fn dev(on: Option<String>) -> i32 {
    match on {
        None => {
            let r = ctl("/dev", json!({}));
            println!("dev mode: {}", if r["dev"].as_bool() == Some(true) { "on" } else { "off" });
            0
        }
        Some(v) => {
            let on = matches!(v.to_lowercase().as_str(), "on" | "1" | "true");
            let r = ctl("/dev", json!({"on": on}));
            println!(
                "dev mode -> {}",
                if r["dev"].as_bool() == Some(true) { "on" } else { "off" }
            );
            0
        }
    }
}

fn add(url: &str, ctx: Option<String>) -> i32 {
    let r = ctl("/add", with_ctx(json!({"url": url}), ctx));
    println!(
        "added {} to ctx {} (new window in instance, port {})",
        repr(url),
        repr(r["ctx"].as_str().unwrap_or_default()),
        r["port"].as_u64().unwrap_or(0)
    );
    0
}

fn close(query: Option<String>, ctx: Option<String>) -> i32 {
    match query {
        Some(q) => {
            let r = ctl("/close_page", with_ctx(json!({"query": q}), ctx));
            println!("closed tab {}", r["closed"].as_str().unwrap_or_default());
        }
        None => {
            let r = ctl("/close_ctx", with_ctx(json!({}), ctx));
            println!("closed ctx {}", repr(r["closed"].as_str().unwrap_or_default()));
        }
    }
    0
}

fn conf(ctx: &str, proxy: Option<String>, ext: Option<String>) -> i32 {
    let mut body = json!({"ctx": ctx});
    // Python resolved the spellings CLI-side: none/off -> clear, default -> clear
    if let Some(p) = proxy {
        let p = if matches!(p.to_lowercase().as_str(), "none" | "off") { String::new() } else { p };
        body["proxy"] = json!(p);
    }
    if let Some(e) = ext {
        let e = if matches!(e.to_lowercase().as_str(), "default" | "") { String::new() } else { e };
        body["extensions"] = json!(e);
    }
    let r = ctl("/conf", body);
    let proxy = r["proxy"].as_str().unwrap_or("(none)");
    let ext = r["extensions"].as_str().unwrap_or("(default surfingkeys)");
    println!("ctx {}: proxy={proxy}  extensions={ext}", repr(ctx));
    0
}

fn col(action: &str, site: Option<String>) -> i32 {
    match action {
        "remember" => {
            let r = ctl("/col", json!({"action": "remember"}));
            println!(
                "remembered {}: {:.3} -> {}",
                r["site"].as_str().unwrap_or_default(),
                r["measured"].as_f64().unwrap_or_default(),
                r["band"].as_str().unwrap_or_default()
            );
        }
        _ => {
            let body = match site {
                Some(s) => json!({"action": "show", "site": s}),
                None => json!({"action": "show"}),
            };
            let r = ctl("/col", body);
            let rows = r["widths"].as_array().cloned().unwrap_or_default();
            if rows.is_empty() {
                println!("no remembered column widths");
            }
            for row in rows {
                println!(
                    "  {:<28} {}",
                    row["site"].as_str().unwrap_or_default(),
                    row["band"].as_str().unwrap_or_default()
                );
            }
        }
    }
    0
}

fn tag(action: &str, tag_id: Option<String>, page_id: Option<String>) -> i32 {
    match action {
        "init" => {
            let r = ctl("/tag_seed", json!({}));
            println!(
                "tag forest seeded ({} new nodes, idempotent)",
                r["seeded"].as_u64().unwrap_or(0)
            );
        }
        "add" | "remove" => {
            let Some(raw_id) = tag_id else {
                println!("tag add/remove needs a tag_id");
                return 1;
            };
            // Python validated the numeric id before any query
            if raw_id.strip_prefix('-').unwrap_or(&raw_id).parse::<u64>().is_err() {
                println!("invalid tag_id {}", repr(&raw_id));
                return 1;
            }
            let id: u64 = raw_id.parse().unwrap();
            let mut body = json!({"tag_id": id, "on": action == "add"});
            if let Some(p) = page_id {
                match p.parse::<u64>() {
                    Ok(v) => body["page_id"] = json!(v),
                    Err(_) => {
                        println!("no page to tag (focused window is not a mudra page, or page_id is invalid)");
                        return 1;
                    }
                }
            }
            match ctl_soft("/tag_set", body) {
                Ok(r) => {
                    let verb = if action == "add" { "assigned" } else { "removed" };
                    println!(
                        "{verb} tag {} -> {} (page#{})",
                        r["tag"].as_str().unwrap_or_default(),
                        r["label"].as_str().unwrap_or_default(),
                        r["page_id"].as_u64().unwrap_or(0)
                    );
                }
                Err(e) => {
                    if e.starts_with("tag ") || e.starts_with("page ") || e.contains("focused window") {
                        // Python printed these lookup failures, not mudrad errors
                        println!("{e}");
                    } else {
                        eprintln!("mudrad: {e}");
                    }
                    return 1;
                }
            }
        }
        _ => return 1,
    }
    0
}

fn ui() -> i32 {
    let r = ctl("/panel", json!({"action": "focus"}));
    if let Some(w) = r["focused"].as_u64() {
        println!("mudra panel: focused existing window #{w}");
    } else {
        println!(
            "mudra panel: http://127.0.0.1:9299/ (pid {})",
            r["spawned"].as_u64().unwrap_or(0)
        );
    }
    0
}
