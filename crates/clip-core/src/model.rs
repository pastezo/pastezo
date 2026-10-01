#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipKind {
    Text,
    Image,
}

impl ClipKind {
    pub(crate) fn to_db(self) -> i64 {
        match self {
            ClipKind::Text => 0,
            ClipKind::Image => 1,
        }
    }

    pub(crate) fn from_db(v: i64) -> Self {
        match v {
            1 => ClipKind::Image,
            _ => ClipKind::Text,
        }
    }
}

/// A stored clipboard entry, as the list needs it. The full text of a text
/// clip is not here (it can be megabytes): see `History::content`.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub id: i64,
    pub uuid: String,
    pub kind: ClipKind,
    /// First `store::PREVIEW_CHARS` characters, or a matching excerpt in search results.
    pub preview: Option<String>,
    /// Set when the whole text is a single web link (see `link_of`).
    pub link: Option<String>,
    pub image_path: Option<String>,
    pub thumb_path: Option<String>,
    pub hash: String,
    pub source_app: Option<String>,
    /// Unix time in milliseconds.
    pub created_at: i64,
    /// Kept at the top of the list.
    pub pinned: bool,
    /// Text that looks like code: shown in a monospaced font (`code::looks_like_code`).
    pub code: bool,
    /// Search-only excerpt start, in UTF-8 bytes of the original text.
    pub preview_offset: usize,
    /// The highlighted range, in UTF-8 bytes relative to `preview`.
    pub match_range: Option<std::ops::Range<usize>>,
}

/// The web address to open when the whole text is a single http(s) link
/// ("https://example.com/a?b", "www.example.com"). Other schemes (file:,
/// javascript:, …) are never treated as links.
pub fn link_of(text: &str) -> Option<String> {
    let text = text.trim();
    // browsers cap URLs well below this; don't scan megabytes of text
    if text.is_empty() || text.len() > 16 * 1024 || text.chars().any(char::is_whitespace) {
        return None;
    }
    let candidate = if text.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("www.")) {
        format!("https://{text}")
    } else {
        text.to_string()
    };
    let url = url::Url::parse(&candidate).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    // "https://foo" without a dot is a typo more often than a link
    let real_host = match url.host()? {
        url::Host::Domain(d) => d.contains('.') || d == "localhost",
        url::Host::Ipv4(_) | url::Host::Ipv6(_) => true,
    };
    real_host.then(|| url.to_string())
}

/// Raw clipboard content, as read from or written to the system clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipContent {
    Text(String),
    /// PNG-encoded bytes.
    Image(Vec<u8>),
}

impl ClipContent {
    pub fn kind(&self) -> ClipKind {
        match self {
            ClipContent::Text(_) => ClipKind::Text,
            ClipContent::Image(_) => ClipKind::Image,
        }
    }

    /// 128-bit XXH3 of the content, 32 hex digits. Not cryptographic: it
    /// finds repeats and names image files. Changing it needs a migration
    /// (`store.rs` v7 moved the history over from BLAKE3).
    pub fn hash(&self) -> String {
        let (tag, bytes) = match self {
            ClipContent::Text(s) => (b't', s.as_bytes()),
            ClipContent::Image(b) => (b'i', b.as_slice()),
        };
        let mut h = xxhash_rust::xxh3::Xxh3::new();
        h.update(&[tag]);
        h.update(bytes);
        format!("{:032x}", h.digest128())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_links() {
        let link = link_of;
        assert_eq!(link("https://example.com/a?b=1#c").as_deref(), Some("https://example.com/a?b=1#c"));
        assert_eq!(link("  http://пример.рф/путь \n").as_deref(), Some("http://xn--e1afmkfd.xn--p1ai/%D0%BF%D1%83%D1%82%D1%8C"));
        assert_eq!(link("www.example.com").as_deref(), Some("https://www.example.com/"));
        assert_eq!(link("WWW.Example.com/x").as_deref(), Some("https://www.example.com/x"));
        assert_eq!(link("http://localhost:3000/").as_deref(), Some("http://localhost:3000/"));
        assert_eq!(link("http://192.168.1.1").as_deref(), Some("http://192.168.1.1/"));
    }

    #[test]
    fn rejects_non_links() {
        for s in [
            "Albuquerque",
            "831 310",
            "see https://example.com",
            "https://example.com and more",
            "example.com",
            "ёжик",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "mailto:a@b.c",
            "https://",
            "https://foo",
            "",
        ] {
            assert_eq!(link_of(s), None, "{s:?}");
        }
    }
}
