//! Conservative, reversible grouping of the currently loaded history.
use clip_core::Clip;
use chrono::{Local, TimeZone};

fn link_key(link: &str) -> Option<String> {
    let mut url = url::Url::parse(link).ok()?;
    let pairs: Vec<(String, String)> = url.query_pairs().filter(|(k, _)| {
        !k.starts_with("utm_") && !matches!(k.as_ref(), "fbclid" | "gclid" | "msclkid")
    }).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    url.set_query(None);
    if !pairs.is_empty() { url.query_pairs_mut().extend_pairs(pairs); }
    Some(url.to_string())
}

fn similar(a: &Clip, b: &Clip) -> bool {
    if a.kind != b.kind || a.image_path.is_some() || a.source_app != b.source_app || a.pinned != b.pinned {
        return false;
    }
    let day = |c: &Clip| Local.timestamp_millis_opt(c.created_at).single().map(|d| d.date_naive());
    if day(a) != day(b) { return false; }
    if let (Some(a), Some(b)) = (&a.link, &b.link) { return link_key(a) == link_key(b); }
    if a.link.is_some() || b.link.is_some() { return false; }
    let (Some(a), Some(b)) = (&a.preview, &b.preview) else { return false };
    // Truncated previews cannot establish that two originals are similar.
    if a.chars().count() >= 2000 || b.chars().count() >= 2000 { return false; }
    let a: Vec<_> = a.split_whitespace().collect();
    let b: Vec<_> = b.split_whitespace().collect();
    if a == b { return true; }
    // Preserve meaningful short values and unrelated prose. A command or sentence
    // must share its first word and at least 80% of words at the same positions.
    a.len() >= 5 && a.len() == b.len() && a[0] == b[0]
        && a.iter().zip(&b).filter(|(x, y)| x == y).count() * 5 >= a.len() * 4
}

pub fn groups(clips: &[Clip], enabled: bool) -> Vec<Vec<usize>> {
    let mut result: Vec<Vec<usize>> = Vec::new();
    for (i, clip) in clips.iter().enumerate() {
        if enabled {
            if let Some(group) = result.iter_mut().find(|g| similar(&clips[g[0]], clip)) {
                group.push(i);
                continue;
            }
        }
        result.push(vec![i]);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use clip_core::{ClipContent, History};
    #[test]
    fn related_variants_keep_their_originals() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path()).unwrap();
        for text in ["git log --oneline --max-count 10", "git log --oneline --max-count 20", "https://example.com/a?utm_source=mail", "https://example.com/a?utm_source=chat", "https://example.com/a?id=2"] {
            h.add(&ClipContent::Text(text.into()), Some("Terminal")).unwrap();
        }
        let clips = h.list(0, 20).unwrap();
        let grouped = groups(&clips, true);
        assert_eq!(grouped.iter().map(Vec::len).collect::<Vec<_>>(), [1, 2, 2]);
        assert_eq!(groups(&clips, false).len(), 5);
        assert_eq!(h.list(0, 20).unwrap().len(), 5);
    }
}
