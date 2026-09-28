//! Settings → Import & Export: the whole history in one JSON file.
//!
//! ```json
//! {"format": "pastezo-history", "version": 1, "clips": [
//!   {"kind": "text", "text": "…", "source": "Safari", "created_at": 1727000000000, "pinned": false},
//!   {"kind": "image", "png": "<base64>", "source": null, "created_at": 1727000000001, "pinned": false}
//! ]}
//! ```
//! `created_at` is Unix time in milliseconds; oldest clip first.
//!
//! Both ways go clip by clip: a history of images can be hundreds of
//! megabytes, and only one clip is in memory at a time.

use std::fmt;
use std::fs;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use clip_core::{ClipContent, History};
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

const FORMAT: &str = "pastezo-history";
const VERSION: u32 = 1;
/// The file extension (and the dialogs' filter).
pub const EXTENSION: &str = "json";

#[derive(Serialize, Deserialize)]
struct Entry {
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    png: Option<String>,
    #[serde(default)]
    source: Option<String>,
    created_at: i64,
    #[serde(default)]
    pinned: bool,
}

/// Writes every clip to `path`. Returns how many were written. The file
/// appears only when complete (written next to it first, then renamed).
pub fn export(history: &History, path: &Path) -> io::Result<usize> {
    let part = PathBuf::from(format!("{}.part", path.display()));
    let written = write_all(history, &part).and_then(|n| fs::rename(&part, path).map(|_| n));
    if written.is_err() {
        let _ = fs::remove_file(&part);
    }
    written
}

fn write_all(history: &History, path: &Path) -> io::Result<usize> {
    let mut out = BufWriter::new(fs::File::create(path)?);
    write!(out, "{{\"format\":\"{FORMAT}\",\"version\":{VERSION},\"clips\":[")?;
    let mut count = 0;
    for id in history.ids().map_err(io::Error::other)? {
        // deleted meanwhile, or its image file is gone: nothing to write
        let Ok(Some(clip)) = history.get(id) else { continue };
        let Ok(content) = history.content(&clip) else { continue };
        let (kind, text, png) = match content {
            ClipContent::Text(t) => ("text", Some(t), None),
            ClipContent::Image(b) => ("image", None, Some(base64::engine::general_purpose::STANDARD.encode(b))),
        };
        let entry = Entry { kind: kind.into(), text, png, source: clip.source_app, created_at: clip.created_at, pinned: clip.pinned };
        out.write_all(if count == 0 { b"\n" } else { b",\n" })?;
        serde_json::to_writer(&mut out, &entry)?;
        count += 1;
    }
    out.write_all(b"\n]}\n")?;
    out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(count)
}

/// Adds the clips of an exported file to the history; clips already there
/// keep their place. Returns how many were added. Clips added before an
/// error stay (importing the same file again skips them).
pub fn import(history: &History, path: &Path) -> io::Result<usize> {
    let file = BufReader::new(fs::File::open(path)?);
    let mut added = 0;
    let mut on_clip = |e: Entry| -> Result<(), String> {
        let content = match (e.kind.as_str(), e.text, e.png) {
            ("text", Some(t), _) => ClipContent::Text(t),
            ("image", _, Some(b)) => match base64::engine::general_purpose::STANDARD.decode(b) {
                Ok(png) => ClipContent::Image(png),
                // damaged: skip it, keep the rest
                Err(_) => return Ok(()),
            },
            // a kind from a newer version, or nothing to add
            _ => return Ok(()),
        };
        match history.import(&content, e.source.as_deref(), e.created_at, e.pinned) {
            Ok(true) => added += 1,
            Ok(false) | Err(clip_core::Error::Image(_)) => {}
            Err(e) => return Err(e.to_string()),
        }
        Ok(())
    };
    let mut de = serde_json::Deserializer::from_reader(file);
    Root { on_clip: &mut on_clip }.deserialize(&mut de).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    de.end().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(added)
}

/// The top-level object: checks `format` and `version` (they come first in
/// our files), then hands each clip of `clips` to `on_clip` as it is read.
struct Root<'a, F> {
    on_clip: &'a mut F,
}

impl<'de, F: FnMut(Entry) -> Result<(), String>> DeserializeSeed<'de> for Root<'_, F> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de, F: FnMut(Entry) -> Result<(), String>> Visitor<'de> for Root<'_, F> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "a Pastezo history")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut ours = false;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "format" => {
                    if map.next_value::<String>()? != FORMAT {
                        return Err(de::Error::custom("not a Pastezo history"));
                    }
                    ours = true;
                }
                "version" => {
                    if map.next_value::<u32>()? > VERSION {
                        return Err(de::Error::custom("made by a newer Pastezo"));
                    }
                }
                "clips" if ours => map.next_value_seed(Clips { on_clip: &mut *self.on_clip })?,
                "clips" => return Err(de::Error::custom("not a Pastezo history")),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if ours { Ok(()) } else { Err(de::Error::custom("not a Pastezo history")) }
    }
}

struct Clips<'a, F> {
    on_clip: &'a mut F,
}

impl<'de, F: FnMut(Entry) -> Result<(), String>> DeserializeSeed<'de> for Clips<'_, F> {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de, F: FnMut(Entry) -> Result<(), String>> Visitor<'de> for Clips<'_, F> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "a list of clips")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(entry) = seq.next_element::<Entry>()? {
            (self.on_clip)(entry).map_err(de::Error::custom)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(40, 20, image::Rgba([200, 30, 30, 255]));
        let mut out = io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn export_then_import_elsewhere_keeps_everything() {
        let dir = tempfile::tempdir().unwrap();
        let from = History::open(&dir.path().join("a")).unwrap();
        from.add(&ClipContent::Text("first\n\"quoted\" ёжик".into()), Some("Notes")).unwrap();
        let (img, _) = from.add(&ClipContent::Image(png()), None).unwrap().unwrap();
        let (last, _) = from.add(&ClipContent::Text("last".into()), Some("Safari")).unwrap().unwrap();
        from.set_pinned(last.id, true).unwrap();
        let file = dir.path().join("history.json");
        assert_eq!(export(&from, &file).unwrap(), 3);
        assert!(!dir.path().join("history.json.part").exists());

        let to = History::open(&dir.path().join("b")).unwrap();
        to.add(&ClipContent::Text("last".into()), None).unwrap(); // already there
        assert_eq!(import(&to, &file).unwrap(), 2);
        let in_to = to.list(0, 10).unwrap();
        assert_eq!(in_to.len(), 3);
        // the imported ones keep their time and source
        let first = in_to.iter().find(|c| c.preview.as_deref() == Some("first\n\"quoted\" ёжик")).unwrap();
        assert_eq!(first.source_app.as_deref(), Some("Notes"));
        let copy = in_to.iter().find(|c| c.hash == img.hash).unwrap();
        assert_eq!(copy.created_at, img.created_at);
        assert_eq!(to.content(copy).unwrap(), ClipContent::Image(png()));
        // again: nothing new
        assert_eq!(import(&to, &file).unwrap(), 0);
    }

    #[test]
    fn pinned_survives_the_trip() {
        let dir = tempfile::tempdir().unwrap();
        let from = History::open(&dir.path().join("a")).unwrap();
        let (c, _) = from.add(&ClipContent::Text("keep me".into()), None).unwrap().unwrap();
        from.set_pinned(c.id, true).unwrap();
        let file = dir.path().join("h.json");
        export(&from, &file).unwrap();
        let to = History::open(&dir.path().join("b")).unwrap();
        import(&to, &file).unwrap();
        let got = to.list(0, 1).unwrap().remove(0);
        assert!(got.pinned && got.created_at == c.created_at);
    }

    #[test]
    fn other_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path()).unwrap();
        let try_import = |text: &str| {
            let file = dir.path().join("x.json");
            fs::write(&file, text).unwrap();
            import(&h, &file)
        };
        assert!(try_import("[1, 2]").is_err());
        assert!(try_import(r#"{"clips": [{"kind": "text", "text": "x", "created_at": 1}]}"#).is_err());
        assert!(try_import(r#"{"format": "other", "clips": []}"#).is_err());
        assert!(try_import(r#"{"format": "pastezo-history", "version": 99, "clips": []}"#).is_err());
        assert!(try_import(r#"{"format": "pastezo-history", "version": 1, "clips": [{"kind": "text""#).is_err());
        assert!(h.list(0, 10).unwrap().is_empty());
        // unknown fields and kinds (a newer version) are skipped
        let newer = r#"{"format": "pastezo-history", "version": 1, "extra": {"a": [1]}, "clips": [
            {"kind": "file", "path": "/x", "created_at": 1},
            {"kind": "text", "text": "ok", "created_at": 2, "color": "red"}]}"#;
        assert_eq!(try_import(newer).unwrap(), 1);
        // a damaged image is skipped and leaves no file; the rest still comes in
        let damaged = r#"{"format": "pastezo-history", "version": 1, "clips": [
            {"kind": "image", "png": "bm90IGEgcG5n", "created_at": 3},
            {"kind": "image", "png": "%%%", "created_at": 4},
            {"kind": "text", "text": "after", "created_at": 5}]}"#;
        assert_eq!(try_import(damaged).unwrap(), 1);
        let pngs = fs::read_dir(dir.path().join("images")).map_or(0, |d| d.count());
        assert_eq!(pngs, 0);
    }
}
