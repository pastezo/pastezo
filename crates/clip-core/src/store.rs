use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row, Transaction};

use crate::code::looks_like_code;
use crate::model::{link_of, Clip, ClipKind};
use crate::search::{fuzzy_matches, trigram_query, Search, SearchKind, FUZZY_MIN_CHARS};
use crate::Result;

/// Characters of text kept in `clips.preview` — what the list shows.
/// The full text lives in `clip_texts` and is read only to copy it back.
pub const PREVIEW_CHARS: usize = 2000;

/// A schema migration: SQL, then optional Rust code in the same transaction.
struct Migration {
    sql: &'static str,
    code: Option<fn(&Transaction) -> rusqlite::Result<()>>,
}

/// Index + 1 is the resulting `user_version`. Never edit a released migration.
const MIGRATIONS: &[Migration] = &[
    Migration {
        sql: r#"
CREATE TABLE clips (
    id          INTEGER PRIMARY KEY,
    uuid        TEXT    NOT NULL UNIQUE,
    kind        INTEGER NOT NULL,
    text        TEXT,
    image_path  TEXT,
    thumb_path  TEXT,
    hash        TEXT    NOT NULL UNIQUE,
    source_app  TEXT,
    created_at  INTEGER NOT NULL
);
CREATE INDEX clips_created ON clips(created_at DESC, id DESC);

-- trigram tokenizer gives substring search ("buq" finds "Albuquerque").
CREATE VIRTUAL TABLE clips_fts USING fts5(
    text, content='clips', content_rowid='id', tokenize='trigram'
);
CREATE TRIGGER clips_ai AFTER INSERT ON clips BEGIN
    INSERT INTO clips_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER clips_ad AFTER DELETE ON clips BEGIN
    INSERT INTO clips_fts(clips_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER clips_au AFTER UPDATE OF text ON clips BEGIN
    INSERT INTO clips_fts(clips_fts, rowid, text) VALUES ('delete', old.id, old.text);
    INSERT INTO clips_fts(rowid, text) VALUES (new.id, new.text);
END;
"#,
        code: None,
    },
    // v2: the list must never read full texts (they can be megabytes).
    // `clips` keeps a short preview and the detected link; the full text moves
    // to `clip_texts`, which is also what full-text search indexes.
    Migration {
        sql: r#"
CREATE TABLE clip_texts (
    id   INTEGER PRIMARY KEY,
    text TEXT NOT NULL
);
INSERT INTO clip_texts (id, text) SELECT id, text FROM clips WHERE text IS NOT NULL;

DROP TRIGGER clips_ai;
DROP TRIGGER clips_ad;
DROP TRIGGER clips_au;
DROP TABLE clips_fts;

ALTER TABLE clips RENAME COLUMN text TO preview;
UPDATE clips SET preview = substr(preview, 1, 2000) WHERE length(preview) > 2000;
ALTER TABLE clips ADD COLUMN link TEXT;

CREATE VIRTUAL TABLE texts_fts USING fts5(
    text, content='clip_texts', content_rowid='id', tokenize='trigram'
);
INSERT INTO texts_fts(texts_fts) VALUES ('rebuild');
CREATE TRIGGER clip_texts_ai AFTER INSERT ON clip_texts BEGIN
    INSERT INTO texts_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER clip_texts_ad AFTER DELETE ON clip_texts BEGIN
    INSERT INTO texts_fts(texts_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER clips_ad AFTER DELETE ON clips BEGIN
    DELETE FROM clip_texts WHERE id = old.id;
END;
"#,
        code: Some(|tx| {
            let mut read = tx.prepare("SELECT id, text FROM clip_texts")?;
            let mut write = tx.prepare("UPDATE clips SET link = ?2 WHERE id = ?1")?;
            let rows = read.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
            for row in rows {
                let (id, text) = row?;
                if let Some(link) = link_of(&text) {
                    write.execute(params![id, link])?;
                }
            }
            Ok(())
        }),
    },
    // v3: the window and the background agent are separate processes. When the
    // window puts a clip back on the clipboard it notes the content hash here,
    // so the agent does not record it as a new copy.
    Migration {
        sql: r#"
CREATE TABLE own_writes (
    hash TEXT    PRIMARY KEY,
    at   INTEGER NOT NULL
);
"#,
        code: None,
    },
    // v4: pinned clips stay at the top of the list (and of search results).
    // The list's index follows the new order; the old one is not used any more.
    Migration {
        sql: r#"
ALTER TABLE clips ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
CREATE INDEX clips_order ON clips(pinned DESC, created_at DESC, id DESC);
DROP INDEX clips_created;
"#,
        code: None,
    },
    // v5: text that looks like code is shown in a monospaced font; decided
    // once on saving, here for what is already in the history.
    Migration {
        sql: "ALTER TABLE clips ADD COLUMN code INTEGER NOT NULL DEFAULT 0;",
        code: Some(|tx| {
            let mut read = tx.prepare("SELECT id, preview FROM clips WHERE kind = 0 AND link IS NULL")?;
            let mut write = tx.prepare("UPDATE clips SET code = 1 WHERE id = ?1")?;
            let rows = read.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)))?;
            for row in rows {
                let (id, preview) = row?;
                if preview.as_deref().is_some_and(looks_like_code) {
                    write.execute([id])?;
                }
            }
            Ok(())
        }),
    },
    // v6: Settings → Statistics: copies counted per 15 minutes and kind. The
    // window adds them up into its own local days; 15 minutes lines up with
    // every time zone (+5:30, +5:45…). Counts only: no content, kept after
    // "Clear all".
    Migration {
        sql: r#"
CREATE TABLE copy_stats (
    bucket INTEGER NOT NULL,
    kind   INTEGER NOT NULL,
    count  INTEGER NOT NULL,
    PRIMARY KEY (bucket, kind)
) WITHOUT ROWID;
"#,
        code: None,
    },
];

/// Statistics bucket: 15 minutes, in ms.
pub(crate) const STATS_BUCKET_MS: i64 = 15 * 60 * 1000;

/// Search with typos: texts checked at most (best candidates of the index)…
const FUZZY_CANDIDATES: u32 = 200;
/// …and characters of each (a typo deep inside a megabyte log is not worth
/// keeping the window waiting).
const FUZZY_SCAN_CHARS: u32 = 20_000;

/// How long a note in `own_writes` stays valid.
const OWN_WRITE_TTL_MS: i64 = 10_000;

/// List columns: everything except the full text.
const COLUMNS: &str =
    "id, uuid, kind, preview, link, image_path, thumb_path, hash, source_app, created_at, pinned, code";

/// The list order: pinned first, then newest first.
const ORDER: &str = "ORDER BY pinned DESC, created_at DESC, id DESC";

/// Fields of a clip that is about to be saved.
pub struct NewClip<'a> {
    pub kind: ClipKind,
    pub text: Option<&'a str>,
    pub image_path: Option<&'a str>,
    pub thumb_path: Option<&'a str>,
    pub hash: &'a str,
    pub source_app: Option<&'a str>,
    /// Unix ms; `None`: now. Set when a clip comes from an exported history.
    pub created_at: Option<i64>,
    pub pinned: bool,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // the WAL file shrinks back to this size after checkpoints instead of staying at its peak
        conn.pragma_update(None, "journal_size_limit", 1024 * 1024)?;
        let mut store = Store { conn };
        store.migrate()?;
        // cheap: refreshes planner statistics only where they are stale
        store.conn.execute_batch("PRAGMA optimize=0x10002;")?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        for (i, m) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(m.sql)?;
            if let Some(code) = m.code {
                code(&tx)?;
            }
            tx.pragma_update(None, "user_version", (i + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Inserts a clip. If the same content (hash) is already in the history,
    /// nothing changes: no duplicate, and the existing entry keeps its place,
    /// time and source. Returns the clip and whether it is new.
    pub fn upsert(&mut self, c: &NewClip) -> Result<(Clip, bool)> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let existing: Option<i64> = tx
            .query_row("SELECT id FROM clips WHERE hash = ?1", [c.hash], |r| r.get(0))
            .optional()?;
        let (id, is_new) = match existing {
            Some(id) => (id, false),
            None => {
                let preview = c.text.map(preview_of);
                let link = c.text.and_then(link_of);
                let code = link.is_none() && preview.as_deref().is_some_and(looks_like_code);
                tx.execute(
                    "INSERT INTO clips (uuid, kind, preview, link, image_path, thumb_path, hash, source_app, created_at, pinned, code)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![
                        uuid::Uuid::new_v4().to_string(),
                        c.kind.to_db(),
                        preview,
                        link,
                        c.image_path,
                        c.thumb_path,
                        c.hash,
                        c.source_app,
                        c.created_at.unwrap_or(now),
                        c.pinned,
                        code,
                    ],
                )?;
                let id = tx.last_insert_rowid();
                if let Some(text) = c.text {
                    tx.execute("INSERT INTO clip_texts (id, text) VALUES (?1, ?2)", params![id, text])?;
                }
                (id, true)
            }
        };
        let clip = tx.query_row(&format!("SELECT {COLUMNS} FROM clips WHERE id = ?1"), [id], from_row)?;
        tx.commit()?;
        Ok((clip, is_new))
    }

    /// Whether content with `hash` is already in the history.
    pub fn contains(&self, hash: &str) -> Result<bool> {
        let mut stmt = self.conn.prepare_cached("SELECT 1 FROM clips WHERE hash = ?1")?;
        Ok(stmt.exists([hash])?)
    }

    /// Every clip id, oldest first (for export: each clip is then read on its own).
    pub fn ids(&self) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare_cached("SELECT id FROM clips ORDER BY created_at, id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get(&self, id: i64) -> Result<Option<Clip>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {COLUMNS} FROM clips WHERE id = ?1"))?;
        Ok(stmt.query_row([id], from_row).optional()?)
    }

    /// The complete text of a text clip.
    pub fn full_text(&self, id: i64) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare_cached("SELECT text FROM clip_texts WHERE id = ?1")?;
        Ok(stmt.query_row([id], |r| r.get(0)).optional()?)
    }

    /// Pinned first, then newest first.
    pub fn list(&self, offset: u32, limit: u32) -> Result<Vec<Clip>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM clips {ORDER} LIMIT ?1 OFFSET ?2"
        ))?;
        let rows = stmt.query_map([limit, offset], from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Clips that fit `q`, in list order (pinned first, then newest first):
    /// first those whose full text has `q.text` as a substring (any case),
    /// then — if there is room left — those that have it with a typo or two.
    pub fn search(&self, q: &Search, limit: u32) -> Result<Vec<Clip>> {
        let text = q.text.trim();
        // the filters, after the text condition's argument
        let mut filter = String::new();
        let mut args: Vec<Value> = Vec::new();
        if let Some(app) = &q.app {
            filter.push_str(" AND source_app LIKE ? ESCAPE '\\'");
            args.push(Value::Text(like_pattern(app)));
        }
        filter.push_str(match q.kind {
            Some(SearchKind::Text) => " AND kind = 0",
            Some(SearchKind::Image) => " AND kind = 1",
            Some(SearchKind::Link) => " AND link IS NOT NULL",
            None => "",
        });
        if let Some(t) = q.after {
            filter.push_str(" AND created_at >= ?");
            args.push(Value::Integer(t));
        }
        if let Some(t) = q.before {
            filter.push_str(" AND created_at < ?");
            args.push(Value::Integer(t));
        }

        // trigram needs at least 3 characters; shorter queries fall back to LIKE
        let (cond, arg) = if text.is_empty() {
            ("1", None)
        } else if text.chars().count() >= 3 {
            ("id IN (SELECT rowid FROM texts_fts WHERE texts_fts MATCH ?)", Some(format!("\"{}\"", text.replace('"', "\"\""))))
        } else {
            ("id IN (SELECT id FROM clip_texts WHERE text LIKE ? ESCAPE '\\')", Some(like_pattern(text)))
        };
        let sql = format!("SELECT {COLUMNS} FROM clips WHERE {cond}{filter} {ORDER} LIMIT ?");
        let all = arg.map(Value::Text).into_iter().chain(args.iter().cloned()).chain([Value::Integer(limit.into())]);
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let mut found: Vec<Clip> = stmt.query_map(params_from_iter(all), from_row)?.collect::<rusqlite::Result<_>>()?;

        // with typos: candidates from the trigram index (texts sharing any three
        // letters in a row with the query, best first), checked here on the
        // start of their text
        let Some(grams) = trigram_query(text).filter(|_| text.chars().count() >= FUZZY_MIN_CHARS) else {
            return Ok(found);
        };
        if found.len() >= limit as usize {
            return Ok(found);
        }
        let sql = format!(
            "SELECT {COLUMNS}, (SELECT substr(text, 1, {FUZZY_SCAN_CHARS}) FROM clip_texts WHERE clip_texts.id = clips.id)
             FROM clips WHERE id IN
               (SELECT rowid FROM texts_fts WHERE texts_fts MATCH ? ORDER BY rank LIMIT {FUZZY_CANDIDATES})
             {filter} {ORDER}"
        );
        let all = std::iter::once(Value::Text(grams)).chain(args);
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params_from_iter(all), |r| Ok((from_row(r)?, r.get::<_, Option<String>>(12)?)))?;
        let exact: std::collections::HashSet<i64> = found.iter().map(|c| c.id).collect();
        for row in rows {
            let (clip, body) = row?;
            if !exact.contains(&clip.id) && body.is_some_and(|b| fuzzy_matches(text, &b)) {
                found.push(clip);
                if found.len() >= limit as usize {
                    break;
                }
            }
        }
        Ok(found)
    }

    /// Notes that this app itself is about to put content with `hash` on the clipboard.
    pub fn mark_own_write(&self, hash: &str) -> Result<()> {
        let now = now_ms();
        self.conn.execute("DELETE FROM own_writes WHERE at < ?1", [now - OWN_WRITE_TTL_MS])?;
        self.conn.execute(
            "INSERT OR REPLACE INTO own_writes (hash, at) VALUES (?1, ?2)",
            params![hash, now],
        )?;
        Ok(())
    }

    /// True (once) if the content with `hash` was put on the clipboard by this app.
    pub fn take_own_write(&self, hash: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM own_writes WHERE hash = ?1 AND at >= ?2",
            params![hash, now_ms() - OWN_WRITE_TTL_MS],
        )?;
        Ok(n > 0)
    }

    /// Changes whenever another connection (the other process) commits.
    pub fn data_version(&self) -> Result<i64> {
        Ok(self.conn.pragma_query_value(None, "data_version", |r| r.get(0))?)
    }

    /// Pins a clip to the top of the list, or unpins it. `false` if it is gone.
    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<bool> {
        Ok(self.conn.execute("UPDATE clips SET pinned = ?2 WHERE id = ?1", params![id, pinned])? > 0)
    }

    /// Deletes a clip (and its text) and returns it, so the caller can remove its files.
    pub fn delete(&self, id: i64) -> Result<Option<Clip>> {
        let clip = self.get(id)?;
        if clip.is_some() {
            self.conn.execute("DELETE FROM clips WHERE id = ?1", [id])?;
        }
        Ok(clip)
    }

    /// Deletes the clips copied before `before_ms`, pinned ones aside, and
    /// returns them, so the caller can remove their files.
    pub fn delete_before(&mut self, before_ms: i64) -> Result<Vec<Clip>> {
        let tx = self.conn.transaction()?;
        let clips = {
            let mut stmt = tx.prepare(&format!("SELECT {COLUMNS} FROM clips WHERE pinned = 0 AND created_at < ?1"))?;
            let rows = stmt.query_map([before_ms], from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        tx.execute("DELETE FROM clips WHERE pinned = 0 AND created_at < ?1", [before_ms])?;
        tx.commit()?;
        Ok(clips)
    }

    /// Counts one copy of `kind` now (Settings → Statistics).
    pub fn count_copy(&self, kind: CopyKind) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT INTO copy_stats (bucket, kind, count) VALUES (?1, ?2, 1)
                 ON CONFLICT (bucket, kind) DO UPDATE SET count = count + 1",
            )?
            .execute(params![now_ms().div_euclid(STATS_BUCKET_MS), kind as u8])?;
        Ok(())
    }

    /// Copies from `from_ms` (inclusive) on, per 15 minutes and kind, oldest first.
    pub fn copy_stats(&self, from_ms: i64) -> Result<Vec<CopyCount>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT bucket, kind, count FROM copy_stats WHERE bucket >= ?1 ORDER BY bucket")?;
        let rows = stmt.query_map([from_ms.div_euclid(STATS_BUCKET_MS)], |r| {
            Ok(CopyCount {
                at_ms: r.get::<_, i64>(0)? * STATS_BUCKET_MS,
                kind: CopyKind::from_db(r.get(1)?),
                count: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every copy counted so far, and the start of the first counted
    /// 15 minutes (`None`: nothing counted yet). The sum reads the whole table:
    /// only buckets with copies have a row (~15 000 a year of daily use), well
    /// under a millisecond, and only while Settings → Statistics is open.
    pub fn copy_total(&self) -> Result<(u64, Option<i64>)> {
        let (total, first): (i64, Option<i64>) =
            self.conn.query_row("SELECT COALESCE(SUM(count), 0), MIN(bucket) FROM copy_stats", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok((total as u64, first.map(|b| b * STATS_BUCKET_MS)))
    }

    /// Deletes the whole history. Returns the image files the removed clips
    /// pointed to (only those: a clip the agent is saving right now keeps its
    /// files).
    pub fn clear(&mut self) -> Result<Vec<String>> {
        let tx = self.conn.transaction()?;
        let files = {
            let mut stmt = tx.prepare("SELECT image_path, thumb_path FROM clips WHERE image_path IS NOT NULL")?;
            let rows = stmt.query_map([], |r| Ok([r.get::<_, Option<String>>(0)?, r.get(1)?]))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().flatten().flatten().collect()
        };
        tx.execute("DELETE FROM clips", [])?;
        tx.commit()?;
        Ok(files)
    }
}

/// What was copied, for the statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyKind {
    Text = 0,
    Image = 1,
    /// a text that is a single web link
    Link = 2,
}

impl CopyKind {
    fn from_db(v: u8) -> Self {
        match v {
            1 => CopyKind::Image,
            2 => CopyKind::Link,
            _ => CopyKind::Text,
        }
    }
}

/// Copies of one kind in one 15-minute bucket starting at `at_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyCount {
    pub at_ms: i64,
    pub kind: CopyKind,
    pub count: u32,
}

/// `%text%` for LIKE, with LIKE's own characters taken literally.
fn like_pattern(text: &str) -> String {
    format!("%{}%", text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

/// First `PREVIEW_CHARS` characters.
fn preview_of(text: &str) -> String {
    match text.char_indices().nth(PREVIEW_CHARS) {
        Some((i, _)) => text[..i].to_string(),
        None => text.to_string(),
    }
}

fn from_row(r: &Row) -> rusqlite::Result<Clip> {
    Ok(Clip {
        id: r.get(0)?,
        uuid: r.get(1)?,
        kind: ClipKind::from_db(r.get(2)?),
        preview: r.get(3)?,
        link: r.get(4)?,
        image_path: r.get(5)?,
        thumb_path: r.get(6)?,
        hash: r.get(7)?,
        source_app: r.get(8)?,
        created_at: r.get(9)?,
        pinned: r.get(10)?,
        code: r.get(11)?,
    })
}

/// Current time in ms; strictly increasing within the process so that
/// items saved in the same millisecond keep their order.
fn now_ms() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static LAST: AtomicI64 = AtomicI64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let mut prev = LAST.load(Ordering::Relaxed);
    loop {
        let next = now.max(prev + 1);
        match LAST.compare_exchange_weak(prev, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(p) => prev = p,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{Search, SearchKind};

    fn text<'a>(s: &'a str, hash: &'a str) -> NewClip<'a> {
        NewClip {
            kind: ClipKind::Text,
            text: Some(s),
            image_path: None,
            thumb_path: None,
            hash,
            source_app: Some("Safari"),
            created_at: None,
            pinned: false,
        }
    }

    #[test]
    fn insert_and_list_newest_first() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert(&text("one", "h1")).unwrap();
        s.upsert(&text("two", "h2")).unwrap();
        let all = s.list(0, 10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].preview.as_deref(), Some("two"));
        assert_eq!(all[0].source_app.as_deref(), Some("Safari"));
    }

    #[test]
    fn duplicate_keeps_its_place() {
        let mut s = Store::open_in_memory().unwrap();
        let (a, new_a) = s.upsert(&text("one", "h1")).unwrap();
        s.upsert(&text("two", "h2")).unwrap();
        let mut again = text("one", "h1");
        again.source_app = Some("Notes");
        let (a2, new_a2) = s.upsert(&again).unwrap();
        assert!(new_a && !new_a2);
        // same entry, untouched: no duplicate, same time and source, still second
        assert_eq!(a, a2);
        let all = s.list(0, 10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].preview.as_deref(), Some("two"));
        assert_eq!(all[1], a);
    }

    #[test]
    fn pagination() {
        let mut s = Store::open_in_memory().unwrap();
        for i in 0..120 {
            s.upsert(&text(&format!("item {i}"), &format!("h{i}"))).unwrap();
        }
        let page = s.list(50, 50).unwrap();
        assert_eq!(page.len(), 50);
        assert_eq!(page[0].preview.as_deref(), Some("item 69"));
        assert_eq!(s.list(100, 50).unwrap().len(), 20);
    }

    #[test]
    fn long_text_keeps_full_copy_but_lists_preview() {
        let mut s = Store::open_in_memory().unwrap();
        let long = "ё".repeat(PREVIEW_CHARS * 3) + " tail-marker";
        let (c, _) = s.upsert(&text(&long, "h1")).unwrap();
        assert_eq!(c.preview.as_ref().unwrap().chars().count(), PREVIEW_CHARS);
        assert_eq!(s.full_text(c.id).unwrap().as_deref(), Some(long.as_str()));
        // search looks at the full text, not the preview
        assert_eq!(s.search(&"tail-marker".into(), 10).unwrap().len(), 1);
    }

    #[test]
    fn code_is_marked() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert(&text("fn main() {\n    run();\n}", "h1")).unwrap();
        s.upsert(&text("Albuquerque", "h2")).unwrap();
        s.upsert(&text("https://example.com/a?b==1", "h3")).unwrap();
        let code: Vec<_> = s.list(0, 10).unwrap().iter().map(|c| c.code).collect();
        assert_eq!(code, [false, false, true]);
    }

    #[test]
    fn link_is_stored() {
        let mut s = Store::open_in_memory().unwrap();
        let (c, _) = s.upsert(&text("https://example.com/x", "h1")).unwrap();
        assert_eq!(c.link.as_deref(), Some("https://example.com/x"));
        let (c, _) = s.upsert(&text("not a link", "h2")).unwrap();
        assert_eq!(c.link, None);
    }

    #[test]
    fn search_substring() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert(&text("Albuquerque", "h1")).unwrap();
        s.upsert(&text("831 310", "h2")).unwrap();
        s.upsert(&text("Lorem ipsum dolor", "h3")).unwrap();

        let r = s.search(&"BUQ".into(), 10).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].preview.as_deref(), Some("Albuquerque"));

        // short query goes through LIKE
        assert_eq!(s.search(&"31".into(), 10).unwrap().len(), 1);
        // quotes and FTS operators are treated literally
        assert!(s.search(&"\"a\" OR b*".into(), 10).unwrap().is_empty());
        assert_eq!(s.search(&"".into(), 10).unwrap().len(), 3);
    }

    #[test]
    fn search_filters() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert(&NewClip { created_at: Some(1_000), source_app: Some("Visual Studio Code"), ..text("fn main() {}", "h1") }).unwrap();
        s.upsert(&NewClip { created_at: Some(2_000), ..text("https://example.com/", "h2") }).unwrap();
        s.upsert(&NewClip { kind: ClipKind::Image, image_path: Some("/i/a.png"), created_at: Some(3_000), source_app: Some("Preview"), ..text("", "h3") }).unwrap();
        let found = |q: Search| s.search(&q, 10).unwrap().into_iter().map(|c| c.hash).collect::<Vec<_>>();
        let q = |f: fn(&mut Search)| {
            let mut q = Search::default();
            f(&mut q);
            q
        };
        assert_eq!(found(q(|q| q.app = Some("studio".into()))), ["h1"]);
        assert_eq!(found(q(|q| q.app = Some("_".into()))), Vec::<String>::new(), "LIKE characters are literal");
        assert_eq!(found(q(|q| q.kind = Some(SearchKind::Image))), ["h3"]);
        assert_eq!(found(q(|q| q.kind = Some(SearchKind::Link))), ["h2"]);
        assert_eq!(found(q(|q| q.kind = Some(SearchKind::Text))), ["h2", "h1"]);
        assert_eq!(found(q(|q| q.after = Some(2_000))), ["h3", "h2"]);
        assert_eq!(found(q(|q| q.before = Some(2_000))), ["h1"]);
        // text and filters together
        assert_eq!(found(Search { app: Some("Safari".into()), ..Search::from("example") }), ["h2"]);
        assert!(found(Search { app: Some("Preview".into()), ..Search::from("example") }).is_empty());
    }

    #[test]
    fn search_with_typos_comes_after_exact_matches() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert(&text("Albuquerque, New Mexico", "h1")).unwrap();
        s.upsert(&text("tickets to Albuqerque", "h2")).unwrap();
        s.upsert(&text("nothing like it", "h3")).unwrap();
        let found = |q: &str| s.search(&q.into(), 10).unwrap().into_iter().map(|c| c.hash).collect::<Vec<_>>();
        // exact first (h2 has it as typed), then the one with a typo
        assert_eq!(found("Albuqerque"), ["h2", "h1"]);
        assert_eq!(found("albuquerque"), ["h1", "h2"]);
        assert_eq!(found("Albukerque"), ["h2", "h1"], "both with typos: newest first");
        // short queries: exact only
        assert!(found("Albx").is_empty());
        // the filters apply to the typo matches too
        assert!(s.search(&Search { app: Some("Notes".into()), ..Search::from("Albukerque") }, 10).unwrap().is_empty());
    }

    #[test]
    fn filtered_list_follows_the_order_index() {
        let s = Store::open_in_memory().unwrap();
        let plan: Vec<String> = s
            .conn
            .prepare(&format!("EXPLAIN QUERY PLAN SELECT {COLUMNS} FROM clips WHERE 1 AND kind = 1 AND created_at >= 5 {ORDER} LIMIT 50"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let plan = plan.join(" | ");
        assert!(plan.contains("clips_order") && !plan.contains("TEMP B-TREE"), "{plan}");
    }

    #[test]
    fn delete_removes_text_and_search_entry() {
        let mut s = Store::open_in_memory().unwrap();
        let (c, _) = s.upsert(&text("Albuquerque", "h1")).unwrap();
        assert_eq!(s.delete(c.id).unwrap().unwrap().id, c.id);
        assert!(s.search(&"buq".into(), 10).unwrap().is_empty());
        assert!(s.search(&"bu".into(), 10).unwrap().is_empty());
        assert!(s.get(c.id).unwrap().is_none());
        assert!(s.full_text(c.id).unwrap().is_none());
    }

    #[test]
    fn pinned_clips_come_first() {
        let mut s = Store::open_in_memory().unwrap();
        let (old, _) = s.upsert(&text("old one", "h1")).unwrap();
        s.upsert(&text("middle", "h2")).unwrap();
        s.upsert(&text("newest", "h3")).unwrap();
        assert!(s.set_pinned(old.id, true).unwrap());
        let order = |v: Vec<Clip>| v.into_iter().map(|c| c.preview.unwrap()).collect::<Vec<_>>();
        assert_eq!(order(s.list(0, 10).unwrap()), ["old one", "newest", "middle"]);
        assert!(s.get(old.id).unwrap().unwrap().pinned);
        // search keeps the same order
        assert_eq!(order(s.search(&"e".into(), 10).unwrap()), ["old one", "newest", "middle"]);
        // a copy of a pinned clip's content leaves it pinned, in place
        s.upsert(&text("old one", "h1")).unwrap();
        assert_eq!(order(s.list(0, 10).unwrap())[0], "old one");
        s.set_pinned(old.id, false).unwrap();
        assert_eq!(order(s.list(0, 10).unwrap()), ["newest", "middle", "old one"]);
        assert!(!s.set_pinned(9999, true).unwrap());
    }

    #[test]
    fn clear_removes_everything() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert(&text("Albuquerque", "h1")).unwrap();
        s.upsert(&NewClip { kind: ClipKind::Image, text: None, image_path: Some("/i/a.png"), thumb_path: Some("/i/a_thumb.jpg"), hash: "h2", source_app: None, created_at: None, pinned: false }).unwrap();
        assert_eq!(s.clear().unwrap(), ["/i/a.png", "/i/a_thumb.jpg"]);
        assert!(s.list(0, 10).unwrap().is_empty());
        assert!(s.search(&"buq".into(), 10).unwrap().is_empty());
        // new clips still go in
        s.upsert(&text("again", "h1")).unwrap();
        assert_eq!(s.list(0, 10).unwrap().len(), 1);
    }

    #[test]
    fn saved_time_and_pin_are_kept() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert(&text("now", "h1")).unwrap();
        let (old, new) = s.upsert(&NewClip { created_at: Some(5), pinned: true, ..text("old", "h2") }).unwrap();
        assert!(new && old.pinned && old.created_at == 5);
        assert!(s.contains("h2").unwrap() && !s.contains("h3").unwrap());
        // oldest first
        assert_eq!(s.ids().unwrap(), [old.id, old.id - 1]);
    }

    #[test]
    fn list_does_not_touch_full_texts() {
        let s = Store::open_in_memory().unwrap();
        let plan: Vec<String> = s
            .conn
            .prepare(&format!(
                "EXPLAIN QUERY PLAN SELECT {COLUMNS} FROM clips {ORDER} LIMIT 50"
            ))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let plan = plan.join(" | ");
        assert!(plan.contains("clips_order"), "{plan}");
        assert!(!plan.contains("clip_texts") && !plan.contains("TEMP B-TREE"), "{plan}");
    }

    #[test]
    fn migration_v1_to_v2_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        {
            // a database as version 1 left it
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATIONS[0].sql).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            let long = "x".repeat(PREVIEW_CHARS + 10);
            for (i, t) in ["Albuquerque", "https://example.com/", long.as_str()].iter().enumerate() {
                conn.execute(
                    "INSERT INTO clips (uuid, kind, text, hash, created_at) VALUES (?1, 0, ?2, ?1, ?3)",
                    params![format!("u{i}"), t, i as i64],
                )
                .unwrap();
            }
        }
        let s = Store::open(&path).unwrap();
        let all = s.list(0, 10).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].preview.as_ref().unwrap().len(), PREVIEW_CHARS);
        assert_eq!(s.full_text(all[0].id).unwrap().unwrap().len(), PREVIEW_CHARS + 10);
        assert_eq!(all[1].link.as_deref(), Some("https://example.com/"));
        assert_eq!(s.search(&"buq".into(), 10).unwrap().len(), 1);
        let v: i64 = s.conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
    }

    #[test]
    fn own_writes_are_taken_once() {
        let s = Store::open_in_memory().unwrap();
        assert!(!s.take_own_write("h").unwrap());
        s.mark_own_write("h").unwrap();
        assert!(s.take_own_write("h").unwrap());
        assert!(!s.take_own_write("h").unwrap());
    }

    #[test]
    fn data_version_sees_other_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        let reader = Store::open(&path).unwrap();
        let mut writer = Store::open(&path).unwrap();
        let before = reader.data_version().unwrap();
        writer.upsert(&text("one", "h1")).unwrap();
        assert_ne!(reader.data_version().unwrap(), before);
        assert_eq!(reader.list(0, 10).unwrap().len(), 1);
    }

    #[test]
    fn migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        Store::open(&path).unwrap().upsert(&text("one", "h1")).unwrap();
        let s = Store::open(&path).unwrap();
        assert_eq!(s.list(0, 10).unwrap().len(), 1);
    }
}
