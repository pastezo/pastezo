//! Shared by the window and capture agent. Memberships are indexed at write time.
use std::sync::LazyLock;
use regex::Regex;
use rusqlite::{params, Connection};
use crate::{Error, Result};

#[derive(Clone, Debug)]
pub struct TagRule {
    pub id: String,
    /// Built-in classifier, or "regex" for a custom rule.
    pub kind: String,
    /// Empty for a built-in's translated default name.
    pub name: String,
    pub color: String,
    pub pattern: String,
    pub enabled: bool,
    pub count: u32,
}

pub(crate) const SCHEMA: &str = "
CREATE TABLE tag_rules (id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL,
 color TEXT NOT NULL, pattern TEXT NOT NULL, enabled INTEGER NOT NULL);
CREATE TABLE clip_tags (tag_id TEXT NOT NULL, clip_id INTEGER NOT NULL, PRIMARY KEY(tag_id, clip_id));
CREATE INDEX clip_tags_clip ON clip_tags(clip_id);
CREATE TRIGGER clips_tags_delete AFTER DELETE ON clips BEGIN DELETE FROM clip_tags WHERE clip_id=old.id; END;
CREATE TRIGGER rules_tags_delete AFTER DELETE ON tag_rules BEGIN DELETE FROM clip_tags WHERE tag_id=old.id; END;
";

pub(crate) fn rules(conn: &Connection) -> rusqlite::Result<Vec<TagRule>> { read_rules(conn, true) }
fn read_rules(conn: &Connection, counts: bool) -> rusqlite::Result<Vec<TagRule>> {
    let count = if counts { "(SELECT count(*) FROM clip_tags WHERE tag_id=r.id)" } else { "0" };
    conn.prepare(&format!("SELECT r.id,kind,name,color,pattern,enabled,{count} FROM tag_rules r ORDER BY r.rowid"))?
        .query_map([], |r| Ok(TagRule { id:r.get(0)?, kind:r.get(1)?, name:r.get(2)?, color:r.get(3)?, pattern:r.get(4)?, enabled:r.get(5)?, count:r.get(6)? }))?.collect()
}

fn compile(pattern: &str) -> std::result::Result<Regex, regex::Error> {
    regex::RegexBuilder::new(pattern).size_limit(1_000_000).build()
}

pub fn validate(rule: &TagRule) -> Result<()> {
    let invalid = |s: &str| Error::InvalidTag(s.into());
    if rule.name.chars().count() > 24 || rule.name.chars().any(|c| c.is_whitespace() || c == '#' || c.is_control()) || (rule.kind == "regex" && rule.name.is_empty()) {
        return Err(invalid("name"));
    }
    if rule.color.len() != 7 || !rule.color.starts_with('#') || !rule.color[1..].bytes().all(|b| b.is_ascii_hexdigit()) { return Err(invalid("color")); }
    if rule.kind == "regex" {
        if rule.pattern.is_empty() || rule.pattern.len() > 1024 { return Err(invalid("pattern")); }
        compile(&rule.pattern).map_err(|_| invalid("pattern"))?;
    } else if !["email", "phone", "website", "code", "json", "color", "image"].contains(&rule.kind.as_str()) {
        return Err(invalid("kind"));
    }
    Ok(())
}

struct Classifier { rule: TagRule, regex: Option<Regex> }
impl Classifier {
    fn new(rule: TagRule) -> Self {
        let regex = (rule.kind == "regex").then(|| compile(&rule.pattern).ok()).flatten();
        Self { rule, regex }
    }
    fn matches(&self, text: &str, image: bool, code: bool) -> bool {
        if !self.rule.enabled { return false; }
        if self.rule.kind == "image" { return image; }
        if image { return false; }
        match self.rule.kind.as_str() {
            "email" => EMAIL.is_match(text),
            "phone" => PHONE.find_iter(text).any(|m| {
                let s = m.as_str().trim();
                let digits = s.bytes().filter(u8::is_ascii_digit).count();
                (8..=15).contains(&digits) && (s.starts_with('+') || (digits >= 10 && (s.contains('(') || s.matches([' ', '-']).count() >= 2)))
                    && !DATE.is_match(s)
            }),
            "website" => crate::model::link_of(text).is_some(),
            "code" => code,
            "json" => {
                let t = text.trim();
                (t.starts_with('{') || t.starts_with('[')) && serde_json::from_str::<serde_json::Value>(t).is_ok()
            },
            "color" => COLOR.is_match(text),
            "regex" => self.regex.as_ref().is_some_and(|r| r.is_match(text)),
            _ => false,
        }
    }
}
static EMAIL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)*\.[a-z]{2,}\b").unwrap());
static PHONE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:\+\d|\b\d)[\d ()-]{6,}\d\b").unwrap());
static DATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}(?:$|\s)").unwrap());
static COLOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?:^|[^\w#])#(?:[a-f0-9]{8}|[a-f0-9]{6}|[a-f0-9]{4}|[a-f0-9]{3})\b").unwrap());

pub(crate) fn migrate(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    for (kind, color) in [("email", "#3578D4"), ("phone", "#258653"), ("website", "#188795"), ("code", "#B66A1E"), ("json", "#8B55C7"), ("color", "#C0438B"), ("image", "#C6534A")] {
        tx.execute("INSERT INTO tag_rules VALUES (?1,?1,'',?2,'',1)", params![kind,color])?;
    }
    reindex(tx)
}

pub(crate) fn index_clip(conn: &Connection, id: i64, text: &str, image: bool, code: bool) -> rusqlite::Result<()> {
    for rule in read_rules(conn, false)? {
        let classifier = Classifier::new(rule);
        if classifier.matches(text, image, code) {
            conn.execute("INSERT OR IGNORE INTO clip_tags VALUES (?1,?2)", params![classifier.rule.id,id])?;
        }
    }
    Ok(())
}

pub(crate) fn refresh(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> { reindex(tx) }

fn reindex(conn: &Connection) -> rusqlite::Result<()> {
    let classifiers: Vec<_> = read_rules(conn, false)?.into_iter().map(Classifier::new).collect();
    conn.execute("DELETE FROM clip_tags", [])?;
    let mut stmt = conn.prepare("SELECT c.id,c.kind,c.code,coalesce(t.text,'') FROM clips c LEFT JOIN clip_texts t ON t.id=c.id")?;
    let mut rows = stmt.query([])?;
    let mut insert = conn.prepare("INSERT INTO clip_tags VALUES (?1,?2)")?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let text: String = row.get(3)?;
        for c in &classifiers {
            if c.matches(&text, row.get::<_,i64>(1)? == 1, row.get(2)?) { insert.execute(params![c.rule.id,id])?; }
        }
    }
    Ok(())
}

pub(crate) fn save(conn: &mut Connection, rule: &TagRule) -> Result<()> {
    validate(rule)?;
    let tx = conn.transaction()?;
    let all = rules(&tx)?;
    let old = all.iter().find(|r| r.id == rule.id);
    if old.is_none() && (rule.kind != "regex" || all.len() >= 39) { return Err(Error::InvalidTag("limit".into())); }
    if old.is_some_and(|r| r.kind != rule.kind) { return Err(Error::InvalidTag("kind".into())); }
    if all.iter().any(|r| r.id != rule.id && !rule.name.is_empty() && r.name.to_lowercase() == rule.name.to_lowercase()) { return Err(Error::InvalidTag("duplicate".into())); }
    let changed = old.is_none_or(|r| r.enabled != rule.enabled || r.pattern != rule.pattern);
    tx.execute("INSERT INTO tag_rules VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET name=excluded.name,color=excluded.color,pattern=excluded.pattern,enabled=excluded.enabled", params![rule.id,rule.kind,rule.name,rule.color,rule.pattern,rule.enabled])?;
    if changed { reindex(&tx)?; }
    tx.commit()?;
    Ok(())
}

pub(crate) fn delete(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM tag_rules WHERE id=?1 AND kind='regex'", [id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{History, ClipContent, Search};
    fn rule(kind: &str) -> TagRule {
        TagRule { id:kind.into(), kind:kind.into(), name:String::new(), color:"#3578D4".into(), pattern:String::new(), enabled:true, count:0 }
    }
    #[test]
    fn builtins_recognize_content_without_common_false_positives() {
        for (kind, yes, no) in [
            ("email", "Пишите: hello+work@example.co.uk", "user@localhost"),
            ("phone", "Телефон: +7 (999) 123-45-67", "2026-10-01"),
            ("website", "https://example.com/docs", "Документы: https://example.com/docs"),
            ("json", " {\"name\":\"Лена\",\"items\":[1,2]} ", "{not json}"),
            ("color", "Основной цвет: #AABBCC", "Ticket #1234567"),
        ] {
            let c = Classifier::new(rule(kind));
            assert!(c.matches(yes,false,false), "{kind}: {yes}");
            assert!(!c.matches(no,false,false), "{kind}: {no}");
            assert!(!c.matches(yes,true,false), "images cannot match text rules");
        }
        let phone = Classifier::new(rule("phone"));
        for s in ["123456789012", "2026-10-01", "123-45-678", "2026-10-01 12:30"] {
            assert!(!phone.matches(s, false, false), "not a phone: {s}");
        }
        assert!(Classifier::new(rule("image")).matches("",true,false));
        assert!(Classifier::new(rule("code")).matches("fn main() {}",false,true));
    }

    #[test]
    fn website_tags_only_standalone_web_addresses() {
        let website = Classifier::new(rule("website"));
        for link in ["https://example.com/docs?q=1#intro", "http://пример.рф/путь", "www.example.com", " \nhttps://example.com\n "] {
            assert!(website.matches(link, false, false), "{link}");
        }
        for text in [
            "Документы: https://example.com/docs",
            "Visit https://example.com for details",
            "https://one.example.com\nhttps://two.example.com",
            "[Docs](https://example.com)",
            "<a href=\"https://example.com\">Docs</a>",
            r#"{"website":"https://example.com"}"#,
            "curl https://example.com/api",
            "/Users/liamka/Desktop/Снимок экрана.png",
            "file:///Users/liamka/Desktop/image.png",
            "hello@example.com",
        ] {
            assert!(!website.matches(text, false, true), "{text}");
        }
    }
    #[test]
    fn custom_rules_reindex_persist_filter_full_history_and_follow_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path()).unwrap();
        let long = format!("{} ORD-1234 contact@example.com", "x ".repeat(1500));
        h.import(&ClipContent::Text(long.clone()),Some("Editor"),1,false).unwrap();
        for i in 0..60 { h.import(&ClipContent::Text(format!("unrelated {i}")), None, i+2, false).unwrap(); }
        let mut r = rule("regex"); r.id="orders".into(); r.name="заказ".into(); r.pattern=r"ORD-\d+".into();
        h.save_tag(&r).unwrap();
        let q = Search { tag_id:Some(r.id.clone()), ..Default::default() };
        let hits = h.search(&q,200).unwrap();
        assert_eq!(hits.len(),1, "full history, including content past preview");
        assert_eq!(h.content(&hits[0]).unwrap(),ClipContent::Text(long));
        let query = Search { text:"contact".into(), ..q.clone() };
        assert_eq!(h.search(&query,200).unwrap().len(),1);
        assert!(h.search(&Search { text:"unrelated".into(), ..q.clone() },200).unwrap().is_empty());
        let second = History::open(dir.path()).unwrap();
        second.add(&ClipContent::Text("ORD-7890".into()), None).unwrap();
        assert_eq!(h.search(&q,200).unwrap().len(),2, "agent applies saved rules");
        r.pattern = "[".into();
        assert!(h.save_tag(&r).is_err());
        assert_eq!(h.search(&q,200).unwrap().len(),2, "invalid change is atomic");
        r.pattern = "ORD-7890".into(); h.save_tag(&r).unwrap();
        assert_eq!(h.search(&q,200).unwrap().len(),1);
        r.enabled=false; h.save_tag(&r).unwrap();
        assert!(h.search(&q,200).unwrap().is_empty());
        r.enabled=true; h.save_tag(&r).unwrap();
        let id = h.search(&q,200).unwrap()[0].id;
        let deleted = h.take(id).unwrap().unwrap();
        assert_eq!(h.tag_rules().unwrap().iter().find(|r|r.id=="orders").unwrap().count,0);
        h.restore(&deleted).unwrap();
        assert_eq!(h.search(&q,200).unwrap().len(),1);
        h.delete_tag("orders").unwrap();
        assert!(h.search(&q,200).unwrap().is_empty());
        h.delete_tag("email").unwrap();
        assert!(h.tag_rules().unwrap().iter().any(|r|r.id=="email"),"built-ins are disabled, not deleted");
    }
}
