//! History interchange — NDJSON, one record per line, format defined HERE.
//!
//! The format is deliberately not a mirror of the store and not a mirror of any
//! browser: an exporter maps its own columns onto these four names, and this
//! side never learns the source schema. A Vivaldi `urls` row is
//! `(url, title, visit_count, last_visit_time)`; a Chromium bookmark export has
//! no counter at all; another mudra writes the same four names it reads.
//!
//! ```text
//! {"url":"https://example.com/","title":"Example","visits":12,"last_visit":"2026-10-24T09:27:42.501Z"}
//! ```
//!
//! - `url` — required, the identity (a second record for the same url folds in)
//! - `title` — optional, may be empty
//! - `visits` — optional counter, defaults to 1
//! - `last_visit` — required, RFC 3339 in UTC (`…Z`); a row with no time is not
//!   history
//!
//! Unknown fields are ignored and blank lines skipped, so a producer can carry
//! extras (`profile`, `first_visit`, a cursor) without a format bump. `#`-leading
//! lines are comments for hand-edited files.

use std::fs;
use std::path::{Path, PathBuf};

use mudra_store::MudraStore;
use serde_json::{json, Value};

/// The file `:export` writes and `:import` reads when the command carries no
/// path. One file per data directory, overwritten in place: an export is a
/// snapshot of the history table, not an append-only log.
pub const DEFAULT_FILE: &str = "history.ndjson";

/// What an import did. `invalid` counts lines that were not a usable record
/// (bad JSON, no url, no parsable `last_visit`); they are skipped so one broken
/// line cannot cost a 60k-row file, but the count is reported rather than hidden.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub rows: usize,
    pub created: usize,
    pub updated: usize,
    pub invalid: usize,
}

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set: no home to expand ~ against".to_string())
}

/// The default exchange file: `<data dir>/history.ndjson`.
pub fn default_path() -> Result<PathBuf, String> {
    Ok(crate::spawn::mudra_home()?.join(DEFAULT_FILE))
}

/// A command's path argument → an absolute path.
///
/// Empty means the default file, `~`/`~/…` mean the user's home, and a relative
/// path is relative to home — not to the daemon's cwd, which is nobody's idea of
/// "here" (the palette is used from inside a browser window).
pub fn resolve_path(arg: &str) -> Result<PathBuf, String> {
    let arg = arg.trim();
    if arg.is_empty() {
        return default_path();
    }
    if arg == "~" {
        return home();
    }
    if let Some(rest) = arg.strip_prefix("~/") {
        return Ok(home()?.join(rest));
    }
    let p = PathBuf::from(arg);
    if p.is_absolute() {
        return Ok(p);
    }
    Ok(home()?.join(p))
}

/// Render an absolute path for the palette: `~` instead of the home prefix, so a
/// completed argument reads the way the user would type it.
pub fn display_path(p: &Path) -> String {
    if let Ok(h) = home()
        && let Ok(rest) = p.strip_prefix(&h)
    {
        let rest = rest.to_string_lossy();
        return if rest.is_empty() {
            "~".to_string()
        } else {
            format!("~/{rest}")
        };
    }
    p.to_string_lossy().into_owned()
}

// ---- RFC 3339 (UTC only) -------------------------------------------------------
// No chrono dependency: the whole need is epoch-ms ↔ `YYYY-MM-DDTHH:MM:SS.mmmZ`,
// and the civil-date arithmetic below is the standard days-from-epoch algorithm.

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Epoch milliseconds → `2026-10-24T09:27:42.501Z`. Always three fraction
/// digits: seconds-precision would make two visits in the same second sort
/// arbitrarily, and a variable-width fraction sorts wrong as text.
pub fn rfc3339(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let millis = ms % 1000;
    let (y, mo, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.fraction]Z` (an explicit offset is accepted too,
/// `+00:00` only — other zones are not silently reinterpreted).
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let s = s.trim();
    let body = s.strip_suffix('Z').or_else(|| s.strip_suffix("+00:00"))?;
    let (date, time) = body.split_once(['T', ' '])?;
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: u32 = dp.next()?.parse().ok()?;
    let d: u32 = dp.next()?.parse().ok()?;
    if dp.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let (clock, frac) = match time.split_once('.') {
        Some((c, f)) => (c, f),
        None => (time, ""),
    };
    let mut tp = clock.split(':');
    let h: i64 = tp.next()?.parse().ok()?;
    let mi: i64 = tp.next()?.parse().ok()?;
    let sec: i64 = tp.next()?.parse().ok()?;
    if tp.next().is_some() || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let millis: u64 = if frac.is_empty() {
        0
    } else {
        let digits: String = frac.chars().take(3).collect();
        let padded = format!("{digits:0<3}");
        padded.parse().ok()?
    };
    let days = days_from_civil(y, mo, d);
    let total = days * 86_400 + h * 3_600 + mi * 60 + sec;
    u64::try_from(total).ok().map(|t| t * 1000 + millis)
}

// ---- export / import -----------------------------------------------------------

/// Write the history table as the interchange format. The file is built beside
/// the target and renamed, so a reader never sees a half-written export.
pub fn export(store: &MudraStore, path: &Path) -> Result<usize, String> {
    let rows = store.history_all().map_err(|e| e.to_string())?;
    let mut body = String::with_capacity(rows.len() * 160);
    for h in &rows {
        let rec = json!({
            "url": h.url,
            "title": h.title,
            "visits": h.visits,
            "last_visit": rfc3339(h.last_at),
        });
        body.push_str(&rec.to_string());
        body.push('\n');
    }
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.tmp",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "export".into())
    ));
    fs::write(&tmp, body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(rows.len())
}

/// Read an interchange file into the history table (fold, not append — see
/// `MudraStore::history_absorb`).
pub fn import(store: &mut MudraStore, path: &Path) -> Result<ImportReport, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut report = ImportReport::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let rec: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                report.invalid += 1;
                continue;
            }
        };
        let url = rec
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        let last_at = rec
            .get("last_visit")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339);
        let (url, last_at) = match (url.is_empty(), last_at) {
            (false, Some(t)) => (url, t),
            _ => {
                report.invalid += 1;
                continue;
            }
        };
        let title = rec.get("title").and_then(Value::as_str).unwrap_or_default();
        let visits = rec
            .get("visits")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .max(1);
        let created = store
            .history_absorb(url, title, visits, last_at)
            .map_err(|e| e.to_string())?;
        report.rows += 1;
        if created {
            report.created += 1;
        } else {
            report.updated += 1;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_round_trips_whole_and_fractional_seconds() {
        for ms in [
            0u64,
            1_000,
            1_792_831_642_501,
            951_782_400_000,
            4_102_444_800_000,
        ] {
            let text = rfc3339(ms);
            assert_eq!(parse_rfc3339(&text), Some(ms), "{ms} -> {text}");
        }
    }

    #[test]
    fn rfc3339_renders_the_epoch_and_a_leap_day() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(951_782_400_123), "2000-02-29T00:00:00.123Z");
    }

    #[test]
    fn parse_rejects_time_without_zone_and_other_offsets() {
        assert_eq!(parse_rfc3339("2026-10-24T09:27:42"), None);
        assert_eq!(parse_rfc3339("2026-10-24T09:27:42+08:00"), None);
        assert_eq!(parse_rfc3339("not a date"), None);
        assert_eq!(
            parse_rfc3339("2026-10-24T09:27:42+00:00"),
            parse_rfc3339("2026-10-24T09:27:42Z")
        );
    }

    #[test]
    fn parse_accepts_short_and_long_fractions() {
        assert_eq!(
            parse_rfc3339("2026-10-24T09:27:42.5Z"),
            parse_rfc3339("2026-10-24T09:27:42.500Z")
        );
        assert_eq!(
            parse_rfc3339("2026-10-24T09:27:42.123456789Z"),
            parse_rfc3339("2026-10-24T09:27:42.123Z")
        );
    }
}