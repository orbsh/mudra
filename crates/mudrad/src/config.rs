//! KDL config loader — port of `mudralib/config.py` (behavioral spec).
//!
//! Two layers: the repo-shipped default `config.kdl` (lives under
//! `mudra_home()`, which the NixOS install symlinks to the repo tree —
//! same file Python resolves via `__file__`'s parent) and the user layer
//! `~/.config/mudra/config.kdl`. User layer overrides per key; the
//! `keys` group merges PER-KEY (a flat dict.update would replace the
//! whole keymap — the bug the Python side caught in testing). Unknown
//! groups/keys are ignored (forward compatibility).
//!
//! The kdl-py quirks this replaces (per the migration note): kdl-rs
//! parses integers as `KdlValue::Integer` and accepts bare identifier
//! arguments, so no post-hoc float coercion table and no quoting
//! requirement. Bool spelling follows the shared file's kdl-py convention
//! (bare `true`/`false`) — the `v1-fallback` feature on the kdl
//! dependency makes that parse. The float→int convergence point becomes
//! a typed read: integer-valued settings accept `Integer` or a
//! whole-number `Float` (a user hand-writing `16.0`) and emit a JSON
//! integer.

use serde_json::{Value, json};

/// group -> (child name -> flat key name), aligned with
/// frontend/shared/lib.js MudraConfig.defaults. Keys outside the table
/// are ignored (forward compatibility, same as Python).
const GROUP_TABLES: &[(&str, &[(&str, &str)])] = &[
    (
        "bar",
        &[
            ("font", "statusFont"),
            ("height", "statusHeight"),
            ("fg", "statusFg"),
            ("bg", "statusBg"),
            ("insertFg", "insertFg"),
            ("insertBg", "insertBg"),
            ("hintFg", "hintFg"),
            ("hintBg", "hintBg"),
        ],
    ),
    ("hint", &[("chars", "hintChars"), ("fontSize", "hintFontSize")]),
    (
        "scroll",
        &[("stepLines", "scrollStepLines"), ("overlapLines", "pageOverlapLines")],
    ),
    ("command", &[("maxCandidates", "maxCandidates")]),
    ("ui", &[("thumbnails", "thumbnails")]),
];

/// Flat keys that must land as JSON integers (the typed-read targets).
const INT_KEYS: &[&str] = &[
    "statusHeight",
    "hintFontSize",
    "scrollStepLines",
    "pageOverlapLines",
    "maxCandidates",
];

/// Render a KDL value as JSON. `wants_int`: integer-valued setting —
/// a whole-number float is admitted and narrowed to i64 (mirrors the
/// Python coerce without its isinstance(float) round-trip quirk; a
/// fractional float on an int key stays a float, same as Python).
fn to_json(v: &kdl::KdlValue, wants_int: bool) -> Value {
    match v {
        kdl::KdlValue::String(s) => json!(s),
        kdl::KdlValue::Bool(b) => json!(b),
        kdl::KdlValue::Integer(i) => i64::try_from(*i)
            .map(Value::from)
            .unwrap_or_else(|_| json!(*i as f64)),
        kdl::KdlValue::Float(f) => {
            if wants_int && f.fract() == 0.0 && *f >= i64::MIN as f64 && *f <= i64::MAX as f64 {
                Value::from(*f as i64)
            } else {
                json!(*f)
            }
        }
        kdl::KdlValue::Null => Value::Null,
    }
}

/// A node's child nodes (empty when it has no `{ }` block).
fn children(node: &kdl::KdlNode) -> &[kdl::KdlNode] {
    node.children().map(|d| d.nodes()).unwrap_or(&[])
}

/// A node's first positional argument (entries without a name; Python
/// `child.args[0]`).
fn first_arg(node: &kdl::KdlNode) -> Option<&kdl::KdlValue> {
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .map(|e| e.value())
}

/// miette's `Display` for `KdlError` is a bare "Failed to parse KDL
/// document" — surface the first diagnostic's message and offset instead
/// (the fields are public; spans are byte offsets into the input).
fn parse_err(e: kdl::KdlError) -> String {
    match e.diagnostics.first() {
        Some(d) => format!(
            "{} (at byte {})",
            d.message.clone().unwrap_or_else(|| "parse error".into()),
            d.span.offset()
        ),
        None => "parse failed".into(),
    }
}

/// KDL text -> flat config dict (Python `parse`). The `keys` group is
/// emitted under a `keybindings` sub-object; `server` parses but is not
/// consumed (the ports are compile-time constants in mudrad).
pub fn parse(text: &str) -> Result<Value, String> {
    let doc: kdl::KdlDocument = text.parse().map_err(parse_err)?;
    let mut out = serde_json::Map::new();
    for group in doc.nodes() {
        let gname = group.name().value();
        if let Some((_, table)) = GROUP_TABLES.iter().find(|(g, _)| *g == gname) {
            for child in children(group) {
                let cname = child.name().value();
                let Some(key) = table.iter().find(|(c, _)| *c == cname).map(|(_, k)| *k) else {
                    continue;
                };
                let Some(value) = first_arg(child) else { continue };
                out.insert(key.to_string(), to_json(value, INT_KEYS.contains(&key)));
            }
        } else if gname == "extensions" {
            // ADR-extension-protocol §1: name -> executable path.
            // Routing metadata only — which processes to run; what they
            // listen to lives in the script's schema (hello-carried).
            let ex = out
                .entry("extensions")
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            let ex = ex.as_object_mut().expect("just created as object");
            for child in children(group) {
                if let Some(value) = first_arg(child) {
                    ex.insert(child.name().value().to_string(), to_json(value, false));
                }
            }
        } else if gname == "keys" {
            // per-key entries: child name = key, first arg = command name
            let kb = out
                .entry("keybindings")
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            let kb = kb.as_object_mut().expect("just created as object");
            for child in children(group) {
                if let Some(value) = first_arg(child) {
                    // keybinding commands are strings; no int coercion
                    // applies (Python `_coerce(child.name)` is a no-op)
                    kb.insert(child.name().value().to_string(), to_json(value, false));
                }
            }
        }
        // `server` and unknown groups: structure only, not consumed
    }
    Ok(Value::Object(out))
}

/// Fold one parsed layer into the accumulator (Python `load`'s body):
/// flat keys replace, `keybindings` and `extensions` merge per-key (an
/// empty group is skipped — `if kb:` truthiness; a layer with no keys
/// never injects the object into the result).
fn apply_layer(base: &mut serde_json::Map<String, Value>, parsed: Value) {
    let Value::Object(mut parsed) = parsed else { return };
    let mut mergeable: Vec<(&str, serde_json::Map<String, Value>)> = Vec::new();
    for key in ["keybindings", "extensions"] {
        if let Some(Value::Object(group)) = parsed.remove(key)
            && !group.is_empty()
        {
            mergeable.push((key, group));
        }
    }
    for (k, v) in parsed {
        base.insert(k, v);
    }
    for (key, group) in mergeable {
        let entry = base
            .entry(key)
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Some(e) = entry.as_object_mut() {
            for (k, v) in group {
                e.insert(k, v);
            }
        }
    }
}

/// Load and merge the two layers (Python `load`). The default layer is
/// required; the user layer is optional. Parse errors propagate with
/// the file location (callers surface them as a failed /config, not a
/// dead daemon).
pub fn load(default_path: &std::path::Path, user_path: &std::path::Path) -> Result<Value, String> {
    let mut out = serde_json::Map::new();
    for (path, required) in [(default_path, true), (user_path, false)] {
        match std::fs::read_to_string(path) {
            Ok(text) => apply_layer(&mut out, parse(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if required {
                    return Err(format!("default config missing: {}", path.display()));
                }
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(Value::Object(out))
}

/// The user layer path, `~/.config/mudra/config.kdl` (Python `USER_PATH`
/// — literal home/.config, no XDG override, same as the Python tree).
pub fn user_config_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".config/mudra/config.kdl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, name: &str, text: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn two_layer_merge_user_overrides_scalars() {
        let dir = tempfile::tempdir().unwrap();
        let default = write(
            dir.path(),
            "default.kdl",
            r##"bar {
    font "12px monospace"
    height 16
    bg "#000000"
}
scroll { stepLines 3 }
"##,
        );
        let user = write(
            dir.path(),
            "user.kdl",
            r##"bar { bg "#ff0000" }
"##,
        );
        let cfg = load(&default, &user).unwrap();
        assert_eq!(cfg["statusFont"], json!("12px monospace")); // repo layer survives
        assert_eq!(cfg["statusBg"], json!("#ff0000")); // user layer wins
        assert_eq!(cfg["scrollStepLines"], json!(3));
    }

    #[test]
    fn keys_group_merges_per_key() {
        // the regression the Python flat-update lost on: one user key
        // change must leave every other binding untouched
        let dir = tempfile::tempdir().unwrap();
        let default = write(
            dir.path(),
            "default.kdl",
            r#"keys {
    j "scrollDown"
    k "scrollUp"
    f "hints"
}
"#,
        );
        let user = write(dir.path(), "user.kdl", "keys { j \"scrollPageDown\" }\n");
        let cfg = load(&default, &user).unwrap();
        let kb = cfg["keybindings"].as_object().unwrap();
        assert_eq!(kb.len(), 3, "group replaced wholesale — per-key merge broken");
        assert_eq!(kb["j"], json!("scrollPageDown"));
        assert_eq!(kb["k"], json!("scrollUp"));
        assert_eq!(kb["f"], json!("hints"));
    }

    #[test]
    fn unknown_groups_and_keys_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let default = write(
            dir.path(),
            "default.kdl",
            r#"server { port 8899 }
future-group { whatever 1 }
bar { height 16 not-a-known-child "x" }
"#,
        );
        let user = write(dir.path(), "user.kdl", "");
        let cfg = load(&default, &user).unwrap();
        // placeholder group parsed but not consumed
        assert!(cfg.get("port").is_none());
        assert!(cfg.get("whatever").is_none());
        assert!(cfg.get("not-a-known-child").is_none());
        assert_eq!(cfg["statusHeight"], json!(16));
    }

    #[test]
    fn int_keys_read_as_json_integers_including_whole_floats() {
        // the float->int convergence is a typed read, not a post-hoc
        // coercion: kdl-rs gives Integer for `16` directly, and a
        // hand-written `16.0` (Float, fract==0) is admitted + narrowed
        let dir = tempfile::tempdir().unwrap();
        let default = write(
            dir.path(),
            "default.kdl",
            r#"bar { height 16 }
hint { fontSize 12.0 }
command { maxCandidates 10 }
"#,
        );
        let user = write(dir.path(), "user.kdl", "");
        let cfg = load(&default, &user).unwrap();
        for key in ["statusHeight", "hintFontSize", "maxCandidates"] {
            let v = &cfg[key];
            assert!(v.is_i64(), "{key} must be a JSON integer, got {v}");
        }
        assert_eq!(cfg["hintFontSize"], json!(12));
    }

    #[test]
    fn missing_default_is_an_error_user_layer_optional() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.kdl");
        let user = write(dir.path(), "user.kdl", "");
        let err = load(&missing, &user).unwrap_err();
        assert!(err.contains("default config missing"), "{err}");
        // user layer absent -> repo layer alone is fine
        let default = write(dir.path(), "default.kdl", "hint { chars \"asdf\" }\n");
        let cfg = load(&default, &missing).unwrap();
        assert_eq!(cfg["hintChars"], json!("asdf"));
    }

    #[test]
    fn bare_identifier_values_parse_without_quotes() {
        // kdl-py's quoting quirk (v1) does not exist in the v2 grammar:
        // a bare identifier is a value
        let dir = tempfile::tempdir().unwrap();
        let default = write(dir.path(), "default.kdl", "keys {\n    gg scrollToTop\n}\n");
        let user = write(dir.path(), "user.kdl", "");
        let cfg = load(&default, &user).unwrap();
        assert_eq!(cfg["keybindings"]["gg"], json!("scrollToTop"));
    }

    #[test]
    fn bare_bool_spelling_parses_via_v1_fallback() {
        // the shared repo file's kdl-py spelling (`thumbnails false`,
        // bare); v2 reserves bare bools, the fallback grammar accepts
        // them. NOTE: v1 and v2 spellings must not mix in ONE file —
        // bare bool is v1-only, bare-identifier values are v2-only, and
        // v1-fallback retries the WHOLE document.
        let dir = tempfile::tempdir().unwrap();
        let default = write(dir.path(), "default.kdl", "ui {\n    thumbnails true\n}\n");
        let user = write(dir.path(), "user.kdl", "");
        let cfg = load(&default, &user).unwrap();
        assert_eq!(cfg["thumbnails"], json!(true));
    }

    #[test]
    fn repo_config_kdl_parses_to_expected_shape() {
        // the shipped default file is part of the contract: load it
        // through the same code path (CARGO_MANIFEST_DIR/../config.kdl)
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config.kdl");
        let cfg = load(&repo, std::path::Path::new("/nonexistent-user-config.kdl")).unwrap();
        assert_eq!(cfg["statusHeight"], json!(16));
        assert_eq!(cfg["hintChars"], json!("asdfghjkl;qwertyuiopzxcv"));
        assert_eq!(cfg["scrollStepLines"], json!(3));
        assert_eq!(cfg["maxCandidates"], json!(10));
        assert_eq!(cfg["thumbnails"], json!(false));
        assert_eq!(cfg["keybindings"]["j"], json!("scrollDown"));
        assert_eq!(cfg["keybindings"].as_object().unwrap().len(), 11);
        assert!(cfg.get("port").is_none(), "server group must not leak");
    }
}
