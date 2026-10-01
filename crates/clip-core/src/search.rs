//! What the search asks for: text and filters (`app:`, `type:`, `after:`,
//! `before:` — parsed by the window, which knows the user's time zone), and
//! the typo-tolerant match used when the exact one finds too little.

/// `type:` in the search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Text,
    Image,
    /// a text that is a single web link
    Link,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Search {
    /// Found as a substring of the full text; with typos after the exact matches.
    pub text: String,
    /// `app:` — part of the source app's name, any case.
    pub app: Option<String>,
    pub kind: Option<SearchKind>,
    pub tag_id: Option<String>,
    /// `after:` — Unix ms, inclusive.
    pub after: Option<i64>,
    /// `before:` — Unix ms, exclusive.
    pub before: Option<i64>,
}

impl Search {
    /// Nothing asked: the whole list.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.app.is_none() && self.kind.is_none() && self.tag_id.is_none() && self.after.is_none() && self.before.is_none()
    }
}

impl From<&str> for Search {
    fn from(text: &str) -> Self {
        Search { text: text.into(), ..Default::default() }
    }
}

/// Shortest query searched with typos: a shorter one would match nearly any text.
pub(crate) const FUZZY_MIN_CHARS: usize = 5;

/// Typos (a letter missed, added or changed) allowed in a query of `n` characters.
fn allowed_typos(n: usize) -> usize {
    match n {
        0..FUZZY_MIN_CHARS => 0,
        FUZZY_MIN_CHARS..=7 => 1,
        _ => 2,
    }
}

/// Whether `text` has a piece within the allowed typos of `query`, any case
/// ("Albuqerque" finds "Albuquerque"). Sellers' algorithm: edit distance to
/// the best-matching piece, one column per character of the text.
pub(crate) fn fuzzy_matches(query: &str, text: &str) -> bool {
    let q: Vec<char> = query.trim().chars().flat_map(char::to_lowercase).collect();
    let k = allowed_typos(q.len());
    if k == 0 {
        return false;
    }
    let mut prev: Vec<usize> = (0..=q.len()).collect();
    let mut cur = vec![0; q.len() + 1];
    for c in text.chars().flat_map(char::to_lowercase) {
        // a match may start anywhere in the text: row 0 stays 0
        for i in 1..=q.len() {
            let replace = prev[i - 1] + usize::from(q[i - 1] != c);
            cur[i] = replace.min(prev[i] + 1).min(cur[i - 1] + 1);
        }
        if cur[q.len()] <= k {
            return true;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    false
}

/// An FTS5 query for the texts sharing any three letters in a row with
/// `query` — the candidates for a match with typos (the trigram index finds
/// them without reading the texts).
pub(crate) fn trigram_query(query: &str) -> Option<String> {
    let chars: Vec<char> = query.trim().chars().flat_map(char::to_lowercase).collect();
    let mut grams: Vec<String> = chars.windows(3).map(|w| w.iter().collect()).collect();
    grams.sort();
    grams.dedup();
    if grams.is_empty() {
        return None;
    }
    Some(grams.iter().map(|g| format!("\"{}\"", g.replace('"', "\"\""))).collect::<Vec<_>>().join(" OR "))
}

/// Translate physical keys between English QWERTY and Russian ЙЦУКЕН.
/// Only a single-script word/phrase is translated; filters are already parsed.
pub(crate) fn other_layout(text: &str) -> Option<String> {
    const EN: &str = "`qwertyuiop[]asdfghjkl;'zxcvbnm,.";
    const RU: &str = "ёйцукенгшщзхъфывапролджэячсмитьбю";
    let latin = text.chars().any(|c| c.is_ascii_alphabetic());
    let russian = text.chars().any(|c| ('а'..='я').contains(&c.to_lowercase().next().unwrap_or(c)) || c == 'ё' || c == 'Ё');
    if latin == russian { return None; }
    let (from, to) = if latin { (EN, RU) } else { (RU, EN) };
    let result: String = text.chars().map(|c| {
        let lower = c.to_lowercase().next().unwrap_or(c);
        let mapped = from.chars().position(|k| k == lower).and_then(|i| to.chars().nth(i)).unwrap_or(c);
        if c.is_uppercase() { mapped.to_uppercase().next().unwrap_or(mapped) } else { mapped }
    }).collect();
    (result != text).then_some(result)
}

pub(crate) struct Excerpt {
    pub text: String,
    pub offset: usize,
    pub matched: std::ops::Range<usize>,
}

/// Find a match in Unicode lowercase text while retaining original byte offsets.
fn match_range(query: &str, text: &str) -> Option<std::ops::Range<usize>> {
    let q: Vec<char> = query.trim().chars().flat_map(char::to_lowercase).collect();
    if q.is_empty() { return None; }
    let lower: String = text.chars().flat_map(char::to_lowercase).collect();
    let needle: String = q.iter().collect();
    if let Some(start) = lower.find(&needle) {
        let end = start + needle.len();
        let mut folded_offset = 0;
        let mut original_start = None;
        for (byte, c) in text.char_indices() {
            let next = folded_offset + c.to_lowercase().map(char::len_utf8).sum::<usize>();
            if original_start.is_none() && next > start { original_start = Some(byte); }
            if next >= end { return Some(original_start.unwrap_or(byte)..byte + c.len_utf8()); }
            folded_offset = next;
        }
    }
    drop(lower);
    let mut folded = Vec::new();
    let mut offsets = Vec::new();
    for (start, c) in text.char_indices().take(20_000) {
        for lower in c.to_lowercase() {
            folded.push(lower);
            offsets.push(start..start + c.len_utf8());
        }
    }
    let k = allowed_typos(q.len());
    if k == 0 || q.len() > 256 { return None; }
    // Sellers distance with start positions, using the same bounded scan as search.
    let mut prev: Vec<(usize, usize)> = (0..=q.len()).map(|i| (i, 0)).collect();
    let mut cur = prev.clone();
    for (i, c) in folded.iter().enumerate() {
        cur[0] = (0, i + 1);
        for j in 1..=q.len() {
            cur[j] = [(prev[j - 1].0 + usize::from(q[j - 1] != *c), prev[j - 1].1),
                (prev[j].0 + 1, prev[j].1), (cur[j - 1].0 + 1, cur[j - 1].1)]
                .into_iter().min_by_key(|x| x.0).unwrap();
        }
        if cur[q.len()].0 <= k && cur[q.len()].1 <= i {
            return Some(offsets[cur[q.len()].1].start..offsets[i].end);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    None
}

pub(crate) fn excerpt(query: &str, text: &str) -> Option<Excerpt> {
    let matched = match_range(query, text)?;
    // A short context keeps the match on screen even in a very long clip.
    let mut start = text[..matched.start].char_indices().rev().nth(55).map_or(0, |(i, _)| i);
    // Avoid opening mid-word or spending all six visible lines on leading context.
    if start > 0 {
        if let Some(i) = text[start..matched.start].find(char::is_whitespace) { start += i; }
    }
    if let Some(i) = text[start..matched.start].rfind('\n') { start += i + 1; }
    start += text[start..matched.start].len() - text[start..matched.start].trim_start().len();
    let end = text[matched.end..].char_indices().nth(180).map_or(text.len(), |(i, _)| matched.end + i);
    Some(Excerpt { text: text[start..end].into(), offset: start, matched: matched.start - start..matched.end - start })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_layout_and_unicode_excerpts() {
        assert_eq!(other_layout("Ghbdtn").as_deref(), Some("Привет"));
        assert_eq!(other_layout("руддщ").as_deref(), Some("hello"));
        assert!(other_layout("hello привет").is_none());
        assert!(other_layout("1234").is_none());
        let e = excerpt("i", "😀İstanbul").unwrap();
        assert_eq!(&e.text[e.matched], "İ");
        let e = excerpt("Albuqerque", "flight to Albuquerque, NM").unwrap();
        assert!(e.text[e.matched].starts_with("Albu"));
        assert!(excerpt("", "text").is_none());
    }

    #[test]
    fn typos_are_forgiven_within_limits() {
        assert!(fuzzy_matches("Albuqerque", "flight to Albuquerque, NM")); // one missed
        assert!(fuzzy_matches("albukerque", "ALBUQUERQUE")); // "qu" typed as "k": two
        assert!(fuzzy_matches("превет", "Привет, мир")); // Cyrillic, any case
        assert!(!fuzzy_matches("albkrk", "Albuquerque")); // too many
        assert!(fuzzy_matches("hello", "say helo")); // five letters: one typo
        assert!(!fuzzy_matches("hello", "say hlo"));
        assert!(!fuzzy_matches("helo", "hello"), "four letters: exact search only");
    }

    #[test]
    fn trigram_candidates() {
        assert_eq!(trigram_query("Abcd").as_deref(), Some("\"abc\" OR \"bcd\""));
        assert_eq!(trigram_query("a\"bc").as_deref(), Some("\"\"\"bc\" OR \"a\"\"b\""));
        assert_eq!(trigram_query("ab"), None);
    }
}
