//! tag-forest UI primitives — render-to-string, zero DOM dependency.
//!
//! Why SSR instead of wasm-in-extension: content-script worlds are under
//! the PAGE's CSP; strict sites (GitHub) lack `wasm-unsafe-eval` and
//! reject `WebAssembly.instantiate` outright. The extension's own origins
//! allow it, but content scripts don't inherit them — so the extension
//! can NOT import the panel's wasm module. Instead this crate renders
//! capsule HTML strings, mudrad returns them on /ctx_status, content.js
//! drops them into the bar, and click events delegate back through the
//! existing SW bridge. Two render entries, one component source:
//! mudrad SSR here, and the R2 leptos panel binding the same types.
//!
//! Shapes ported from frontend/shared/tags.js (Capsule/Chip/RankAxis) and
//! the bar's read-only renderer in lib.js. The bar capsule is read-only
//! display (segment menus come later); the interactive buttons are
//! present only when the host supplies their callbacks — same protocol
//! as the JS version.

/// Root tag name → rank-axis glyph (port of ui.py `ROOT_AXIS`). The
/// axis lives here, not in any host: the SSR bar, the mudrad `/forest`
/// payload, and the leptos panel all read the same table — adding a
/// rank root means adding one line in this crate, single source.
pub fn root_axis(name: &str) -> Option<&'static str> {
    match name {
        "importance" => Some("★"),
        "quality" => Some("♥"),
        "urgency" => Some("🔥"),
        _ => None,
    }
}

/// One path-segment string of a tag: `state::unread` splits into segs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagPath<'a>(pub &'a str);

impl<'a> TagPath<'a> {
    pub fn segments(&self) -> impl Iterator<Item = &'a str> {
        self.0.split("::")
    }
}

/// Capsule props (tags.js Capsule): optional callbacks mark which
/// interactive affordances the host wants rendered; SSR here renders the
/// read-only shape unless a host builds with actions.
#[derive(Debug, Clone, Copy, Default)]
pub struct Capsule<'a> {
    pub path: &'a str,
    pub remove: bool,
    pub add_child: bool,
}

/// One selectable chip (tags.js Chip): the `on` state comes from the host.
#[derive(Debug, Clone, Copy)]
pub struct Chip<'a> {
    pub path: &'a str,
    pub on: bool,
}

/// One node on a rank axis (tags.js RankAxis): `children` are the ranked
/// tags, `selected_rank` lights the first N pips.
#[derive(Debug, Clone)]
pub struct RankAxis<'a> {
    pub name: &'a str,
    pub alias: Option<&'a str>,
    pub axis: &'a str,
    pub selected_rank: u8,
}

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Capsule → HTML string. Byte-shape contract with the JS renderer:
/// `span.capsule` wrapping one `span.seg` per path level, the last also
/// `leaf`. The bar's read-only renderer never shows the buttons, so this
/// is the exact string the current UI needs; `remove`/`add_child` render
/// the same ✕/＋ spans tags.js uses when a host asks for them.
pub fn capsule_html(c: &Capsule) -> String {
    let segs: Vec<&str> = TagPath(c.path).segments().collect();
    let last = segs.len().saturating_sub(1);
    let mut out = String::from("<span class=\"capsule\">");
    if c.remove {
        out.push_str("<span class=\"cp-x\" title=\"Delete this tag\">✕</span>");
    }
    for (i, seg) in segs.iter().enumerate() {
        out.push_str("<span class=\"seg");
        if i == last {
            out.push_str(" leaf");
        }
        out.push_str("\">");
        out.push_str(&esc(seg));
        out.push_str("</span>");
    }
    if c.add_child {
        out.push_str("<span class=\"cp-close\" title=\"Add child\">＋</span>");
    }
    out.push_str("</span>");
    out
}

/// A row of capsules, one per page tag path (the bar renders all tags of
/// the current page). Empty input renders the empty string.
pub fn capsules_html(paths: &[&str]) -> String {
    paths
        .iter()
        .map(|p| capsule_html(&Capsule { path: p, remove: false, add_child: false }))
        .collect()
}

/// Chip → HTML string (`button.chip`, `.on` when selected).
pub fn chip_html(c: &Chip) -> String {
    format!(
        "<button class=\"chip{}\">{}</button>",
        if c.on { " on" } else { "" },
        esc(c.path)
    )
}

/// Rank axis → HTML string: five pips, the first `selected_rank` lit,
/// title = `name（alias）` when an alias exists (same shape as tags.js).
pub fn rank_axis_html(a: &RankAxis) -> String {
    let title = match a.alias {
        Some(al) => format!("{}（{}）", a.name, al),
        None => a.name.to_string(),
    };
    let mut out = format!("<span class=\"rank\" title=\"{}\">", esc(&title));
    for k in 1..=5u8 {
        out.push_str("<span class=\"rk");
        if k <= a.selected_rank {
            out.push_str(" on");
        }
        out.push_str("\">");
        out.push_str(&esc(a.axis));
        out.push_str("</span>");
    }
    out.push_str("</span>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capsule_matches_the_bar_readonly_shape() {
        // The golden shape from frontend/shared/lib.js Capsule():
        // span.capsule > span.seg (+ leaf on the last), no buttons.
        let h = capsule_html(&Capsule { path: "state::unread", remove: false, add_child: false });
        assert_eq!(
            h,
            "<span class=\"capsule\"><span class=\"seg\">state</span>\
             <span class=\"seg leaf\">unread</span></span>"
        );
    }

    #[test]
    fn capsule_single_segment_is_leaf() {
        let h = capsule_html(&Capsule { path: "inbox", ..Default::default() });
        assert_eq!(h, "<span class=\"capsule\"><span class=\"seg leaf\">inbox</span></span>");
    }

    #[test]
    fn capsule_with_actions_renders_buttons() {
        let h = capsule_html(&Capsule { path: "work", remove: true, add_child: true });
        assert!(h.starts_with("<span class=\"capsule\"><span class=\"cp-x\""));
        assert!(h.ends_with("＋</span></span>"));
    }

    #[test]
    fn capsules_join_into_a_row() {
        let paths = ["state::unread", "work"];
        let joined = capsules_html(&paths);
        assert_eq!(joined.matches("class=\"capsule\"").count(), 2);
        assert_eq!(capsules_html(&[]), "");
    }

    #[test]
    fn html_escapes_path_text() {
        // tag paths are user data: `<script>` must not survive the round
        // trip into the bar (innerHTML assignment site)
        let h = capsule_html(&Capsule { path: "a<b>&\"c\"", ..Default::default() });
        assert!(h.contains("a&lt;b&gt;&amp;&quot;c&quot;"));
        // exactly the two structural tags, nothing user-injected
        assert_eq!(h.matches("<span").count(), 2);
    }

    #[test]
    fn chip_state_from_props() {
        assert_eq!(chip_html(&Chip { path: "x", on: true }), "<button class=\"chip on\">x</button>");
        assert_eq!(chip_html(&Chip { path: "x", on: false }), "<button class=\"chip\">x</button>");
    }

    #[test]
    fn rank_axis_pips_and_title() {
        let h = rank_axis_html(&RankAxis {
            name: "深度",
            alias: Some("deep work"),
            axis: "●",
            selected_rank: 2,
        });
        assert!(h.contains("title=\"深度（deep work）\""));
        assert_eq!(h.matches("class=\"rk on\"").count(), 2);
        assert_eq!(h.matches("class=\"rk\"").count(), 3);
        assert_eq!(h.matches("●").count(), 5);
    }
}
