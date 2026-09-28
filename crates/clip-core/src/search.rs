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
    /// `after:` — Unix ms, inclusive.
    pub after: Option<i64>,
    /// `before:` — Unix ms, exclusive.
    pub before: Option<i64>,
}

impl Search {
    /// Nothing asked: the whole list.
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.app.is_none() && self.kind.is_none() && self.after.is_none() && self.before.is_none()
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

#[cfg(test)]
mod tests {
    use super::*;

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
