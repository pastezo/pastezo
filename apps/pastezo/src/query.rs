//! The search field's text → `clip_core::Search`: the words to find plus
//! filters, written as in mail search:
//!
//! | filter | finds |
//! |---|---|
//! | `app:Safari`, `app:"Visual Studio"` | copied from an app whose name contains that |
//! | `type:text`, `type:image`, `type:link` | that kind of clip |
//! | `after:2026-09-01`, `after:today` | copied on that day or later |
//! | `before:2026-09-01`, `before:yesterday` | copied before that day |
//!
//! Filter names are English in every language (like the operators of mail
//! and file search). Days are the user's (local midnight). A filter that does
//! not parse stays part of the text: `note:` or `https://…` are just text.

use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone};
use clip_core::{Search, SearchKind};

pub fn parse(input: &str, now: DateTime<Local>) -> Search {
    let mut search = Search::default();
    let mut words: Vec<&str> = Vec::new();
    let mut rest = input;
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let (token, value, after) = next_token(rest);
        rest = after;
        let applied = match token.split_once(':').map(|(k, _)| k.to_ascii_lowercase()).as_deref() {
            Some("app") if !value.trim_matches('"').is_empty() => {
                search.app = Some(value.trim_matches('"').to_string());
                true
            }
            Some("type") => kind(value).map(|k| search.kind = Some(k)).is_some(),
            Some("after") => day_start(value, now).map(|t| search.after = Some(t)).is_some(),
            Some("before") => day_start(value, now).map(|t| search.before = Some(t)).is_some(),
            _ => false,
        };
        if !applied {
            words.push(token);
        }
    }
    search.text = words.join(" ");
    search
}

/// The token at the start of `s` (up to a space; `key:"…"` up to the closing
/// quote), the filter value inside it, and what follows.
fn next_token(s: &str) -> (&str, &str, &str) {
    let key_end = s.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(s.len());
    if key_end > 0 && s[key_end..].starts_with(":\"") {
        let open = key_end + 2;
        if let Some(len) = s[open..].find('"') {
            let close = open + len;
            return (&s[..close + 1], &s[open..close], &s[close + 1..]);
        }
    }
    let end = s.find(char::is_whitespace).unwrap_or(s.len());
    let token = &s[..end];
    let value = token.split_once(':').map_or("", |(_, v)| v);
    (token, value, &s[end..])
}

fn kind(value: &str) -> Option<SearchKind> {
    match value.to_ascii_lowercase().trim_end_matches('s') {
        "text" => Some(SearchKind::Text),
        "image" | "img" => Some(SearchKind::Image),
        "link" | "url" => Some(SearchKind::Link),
        _ => None,
    }
}

/// Unix ms of the local midnight that starts the day `value` names:
/// `2026-09-01`, `today`, `yesterday`.
fn day_start(value: &str, now: DateTime<Local>) -> Option<i64> {
    let today = now.date_naive();
    let day = match value.to_ascii_lowercase().as_str() {
        "today" => today,
        "yesterday" => today - Duration::days(1),
        v => NaiveDate::parse_from_str(v, "%Y-%m-%d").ok()?,
    };
    Local.from_local_datetime(&day.and_hms_opt(0, 0, 0)?).earliest().map(|t| t.timestamp_millis())
}

/// Calendar-day boundaries, not a fixed 24-hour duration (DST can change it).
pub fn date_bounds(day: NaiveDate) -> Option<(i64, i64)> {
    let midnight = |d: NaiveDate| Local.from_local_datetime(&d.and_hms_opt(0, 0, 0)?).earliest().map(|t| t.timestamp_millis());
    Some((midnight(day)?, midnight(day.succ_opt()?)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 28, 15, 30, 0).unwrap()
    }

    fn midnight(y: i32, m: u32, d: u32) -> i64 {
        Local.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap().timestamp_millis()
    }

    #[test]
    fn plain_text_stays_text() {
        let q = parse("  hello   world ", now());
        assert_eq!(q, Search::from("hello world"));
        for text in ["https://example.com/a:b", "note: buy milk", "type:unknown", "after:someday", "app:", "12:30"] {
            assert_eq!(parse(text, now()).text, text, "{text}");
        }
    }

    #[test]
    fn filters() {
        let q = parse("invoice app:Safari type:link after:2026-09-01 before:today", now());
        assert_eq!(q.text, "invoice");
        assert_eq!(q.app.as_deref(), Some("Safari"));
        assert_eq!(q.kind, Some(SearchKind::Link));
        assert_eq!(q.after, Some(midnight(2026, 9, 1)));
        assert_eq!(q.before, Some(midnight(2026, 9, 28)));

        let q = parse(r#"APP:"Visual Studio Code" fn main TYPE:Images"#, now());
        assert_eq!((q.text.as_str(), q.app.as_deref(), q.kind), ("fn main", Some("Visual Studio Code"), Some(SearchKind::Image)));
        assert_eq!(parse("after:yesterday", now()).after, Some(midnight(2026, 9, 27)));
        // only filters: a search, with no text
        let q = parse("type:image", now());
        assert!(q.text.is_empty() && !q.is_empty());
        // an unclosed quote: up to the next space
        assert_eq!(parse(r#"app:"Visual Studio"#, now()).app.as_deref(), Some("Visual"));
    }
}
