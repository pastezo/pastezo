use std::fs;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use image::codecs::jpeg::JpegEncoder;
use image::ImageFormat;

use crate::model::{Clip, ClipContent};
use crate::search::Search;
use crate::store::{CopyCount, CopyKind, NewClip, Store};
use crate::{Error, Result};

/// The largest clip kept, text or image (as copied): bigger copies are not
/// saved at all — nothing a history is for, and megabytes on every search.
pub const MAX_CLIP_BYTES: usize = 5 * 1024 * 1024;

/// Settings → General → Keep clips (`History::keep_days`), in the data folder.
const KEEP_DAYS_FILE: &str = "keep-days";
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Longest side of an image preview, in pixels (2x for retina).
const THUMB_SIZE: u32 = 720;
/// Previews of opaque images are JPEG: several times smaller than PNG and
/// indistinguishable at this size. Transparent ones stay PNG.
const THUMB_JPEG_QUALITY: u8 = 82;

/// Where the history lives: the same folder for the window and the agent
/// (and the one the Tauri app used). `None` where the OS has no such folder
/// (Android: the app passes its own).
pub fn data_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("app.pastezo"))
}

/// Clipboard history: the database plus image files on disk.
///
/// Shared between threads as is: slow work (hashing, decoding, previews)
/// runs outside the database lock, so the UI is never blocked by a big image
/// being saved.
pub struct History {
    store: Mutex<Store>,
    images_dir: PathBuf,
    /// the data folder: `keep-days` is there
    dir: PathBuf,
}

impl History {
    /// Opens (or creates) the history in `data_dir`.
    pub fn open(data_dir: &Path) -> Result<Self> {
        let images_dir = data_dir.join("images");
        fs::create_dir_all(&images_dir)?;
        let store = Mutex::new(Store::open(&data_dir.join("clips.sqlite"))?);
        Ok(History { store, images_dir, dir: data_dir.to_path_buf() })
    }

    /// Saves new clipboard content. Returns `None` for content that is not
    /// kept: empty text, or more than `MAX_CLIP_BYTES`.
    pub fn add(&self, content: &ClipContent, source_app: Option<&str>) -> Result<Option<(Clip, bool)>> {
        if !keeps(content) {
            return Ok(None);
        }
        let hash = content.hash();
        {
            let store = self.store.lock().unwrap();
            // every copy counts, a repeat and a copy from our own window too;
            // a failed count never loses the clip
            let _ = store.count_copy(copy_kind(content));
            // put there by our own window: already in the history, keep its place
            if store.take_own_write(&hash)? {
                return Ok(None);
            }
        }
        self.save(content, &hash, source_app, None, false).map(Some)
    }

    /// Adds a clip from an exported history with its own time and pin.
    /// `false` if it is not added: already in the history (then it keeps its
    /// place), empty, or over `MAX_CLIP_BYTES`.
    pub fn import(&self, content: &ClipContent, source_app: Option<&str>, created_at: i64, pinned: bool) -> Result<bool> {
        if !keeps(content) {
            return Ok(false);
        }
        let hash = content.hash();
        if self.store.lock().unwrap().contains(&hash)? {
            return Ok(false);
        }
        Ok(self.save(content, &hash, source_app, Some(created_at), pinned)?.1)
    }

    /// Settings → Statistics: copies from `from_ms` on, per 15 minutes and kind.
    pub fn copy_stats(&self, from_ms: i64) -> Result<Vec<CopyCount>> {
        self.store.lock().unwrap().copy_stats(from_ms)
    }

    /// Every copy counted so far, and when counting started.
    pub fn copy_total(&self) -> Result<(u64, Option<i64>)> {
        self.store.lock().unwrap().copy_total()
    }

    /// Every clip id, oldest first.
    pub fn ids(&self) -> Result<Vec<i64>> {
        self.store.lock().unwrap().ids()
    }

    fn save(
        &self,
        content: &ClipContent,
        hash: &str,
        source_app: Option<&str>,
        created_at: Option<i64>,
        pinned: bool,
    ) -> Result<(Clip, bool)> {
        let (text, image_path, thumb_path) = match content {
            ClipContent::Text(s) => (Some(s.as_str()), None, None),
            ClipContent::Image(png) => {
                let (img, thumb) = self.save_image(hash, png)?;
                (None, Some(img), Some(thumb))
            }
        };
        let image_path = image_path.as_ref().map(|p| p.to_string_lossy());
        let thumb_path = thumb_path.as_ref().map(|p| p.to_string_lossy());
        self.store.lock().unwrap().upsert(&NewClip {
            kind: content.kind(),
            text,
            image_path: image_path.as_deref(),
            thumb_path: thumb_path.as_deref(),
            hash,
            source_app,
            created_at,
            pinned,
        })
    }

    fn save_image(&self, hash: &str, png: &[u8]) -> Result<(PathBuf, PathBuf)> {
        let img_path = self.images_dir.join(format!("{hash}.png"));
        let jpeg = self.images_dir.join(format!("{hash}_thumb.jpg"));
        let png_thumb = self.images_dir.join(format!("{hash}_thumb.png"));
        let write_image = || -> Result<()> {
            if !img_path.exists() {
                fs::write(&img_path, png)?;
            }
            Ok(())
        };
        for existing in [&jpeg, &png_thumb] {
            if existing.exists() {
                write_image()?;
                return Ok((img_path.clone(), existing.clone()));
            }
        }
        // decoded before anything is written: a broken image leaves no file behind
        let thumb = image::load_from_memory_with_format(png, ImageFormat::Png)?
            .thumbnail(THUMB_SIZE, THUMB_SIZE)
            .into_rgba8();
        write_image()?;
        if thumb.pixels().all(|p| p[3] == 255) {
            let rgb = image::DynamicImage::ImageRgba8(thumb).into_rgb8();
            let mut out = io::BufWriter::new(fs::File::create(&jpeg)?);
            JpegEncoder::new_with_quality(&mut out, THUMB_JPEG_QUALITY).encode_image(&rgb)?;
            Ok((img_path, jpeg))
        } else {
            thumb.save_with_format(&png_thumb, ImageFormat::Png)?;
            Ok((img_path, png_thumb))
        }
    }

    pub fn get(&self, id: i64) -> Result<Option<Clip>> {
        self.store.lock().unwrap().get(id)
    }

    pub fn list(&self, offset: u32, limit: u32) -> Result<Vec<Clip>> {
        self.store.lock().unwrap().list(offset, limit)
    }

    pub fn search(&self, query: &Search, limit: u32) -> Result<Vec<Clip>> {
        self.store.lock().unwrap().search(query, limit)
    }

    /// Deletes a clip with its files, for good.
    pub fn delete(&self, id: i64) -> Result<Option<Clip>> {
        let Some(d) = self.take(id)? else { return Ok(None) };
        let clip = d.clip.clone();
        self.discard(d);
        Ok(Some(clip))
    }

    /// Takes a clip out of the history, keeping what `restore` needs to put it
    /// back: its full text here, its image files on disk until `discard`.
    pub fn take(&self, id: i64) -> Result<Option<Deleted>> {
        let store = self.store.lock().unwrap();
        let text = store.full_text(id)?;
        Ok(store.delete(id)?.map(|clip| Deleted { clip, text }))
    }

    /// Puts a taken clip back, with its time, source and pin (under a new id).
    /// If the same content was copied again meanwhile, that clip stays instead.
    pub fn restore(&self, d: &Deleted) -> Result<Clip> {
        let c = &d.clip;
        let (clip, _) = self.store.lock().unwrap().upsert(&NewClip {
            kind: c.kind,
            text: d.text.as_deref(),
            image_path: c.image_path.as_deref(),
            thumb_path: c.thumb_path.as_deref(),
            hash: &c.hash,
            source_app: c.source_app.as_deref(),
            created_at: Some(c.created_at),
            pinned: c.pinned,
        })?;
        Ok(clip)
    }

    /// A taken clip will not come back: its image files go, unless the same
    /// image was copied again meanwhile (the files are named by content).
    pub fn discard(&self, d: Deleted) {
        if matches!(self.store.lock().unwrap().contains(&d.clip.hash), Ok(false)) {
            for p in [&d.clip.image_path, &d.clip.thumb_path].into_iter().flatten() {
                let _ = fs::remove_file(p);
            }
        }
    }

    /// Settings → General → Keep clips: for how many days clips stay
    /// (`None`: for ever). Saved as the `keep-days` file, so the agent reads
    /// it too; a missing or broken file is "for ever".
    pub fn keep_days(&self) -> Option<u32> {
        let text = fs::read_to_string(self.dir.join(KEEP_DAYS_FILE)).ok()?;
        text.trim().parse().ok().filter(|&d| d > 0)
    }

    pub fn set_keep_days(&self, days: Option<u32>) -> Result<()> {
        let file = self.dir.join(KEEP_DAYS_FILE);
        match days {
            Some(d) => fs::write(file, d.to_string())?,
            None => match fs::remove_file(file) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            },
        }
        Ok(())
    }

    /// Deletes the clips older than `keep_days` (pinned ones stay) with their
    /// files; returns how many went.
    pub fn forget_old(&self, now_ms: i64) -> Result<usize> {
        let Some(days) = self.keep_days() else { return Ok(0) };
        let clips = self.store.lock().unwrap().delete_before(now_ms - i64::from(days) * DAY_MS)?;
        let n = clips.len();
        for clip in clips {
            self.discard(Deleted { clip, text: None });
        }
        Ok(n)
    }

    /// Pins a clip to the top of the list, or unpins it.
    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<bool> {
        self.store.lock().unwrap().set_pinned(id, pinned)
    }

    /// Deletes the whole history with its image files.
    pub fn clear(&self) -> Result<()> {
        let files = self.store.lock().unwrap().clear()?;
        for p in files {
            let _ = fs::remove_file(p);
        }
        Ok(())
    }

    /// Call right before putting `content` on the clipboard from the window,
    /// so the background agent does not record it again.
    pub fn mark_own_write(&self, content: &ClipContent) -> Result<()> {
        self.store.lock().unwrap().mark_own_write(&content.hash())
    }

    /// Changes whenever the other process (agent or window) wrote to the history.
    pub fn data_version(&self) -> Result<i64> {
        self.store.lock().unwrap().data_version()
    }

    /// What to put on the clipboard for clip `id`: all of it, or only the bytes
    /// `part` of its text (a selection made in the window; the list preview
    /// is the start of the full text, so its offsets are valid here too).
    /// `None` if the clip is gone or `part` is not a piece of its text.
    pub fn content_of(&self, id: i64, part: Option<Range<usize>>) -> Result<Option<ClipContent>> {
        let Some(clip) = self.get(id)? else { return Ok(None) };
        let content = self.content(&clip)?;
        Ok(match (content, part) {
            (c, None) => Some(c),
            (ClipContent::Text(s), Some(r)) => s.get(r).filter(|t| !t.is_empty()).map(|t| ClipContent::Text(t.into())),
            (ClipContent::Image(_), Some(_)) => None,
        })
    }

    /// Full content of a stored clip, ready to put back on the clipboard.
    pub fn content(&self, clip: &Clip) -> Result<ClipContent> {
        if let Some(p) = &clip.image_path {
            return Ok(ClipContent::Image(fs::read(p)?));
        }
        let text = self.store.lock().unwrap().full_text(clip.id)?;
        text.map(ClipContent::Text).ok_or(Error::Missing(clip.id))
    }
}

/// A clip taken out of the history (`History::take`) that can still be put
/// back (`History::restore`) until it is discarded.
pub struct Deleted {
    clip: Clip,
    text: Option<String>,
}

impl Deleted {
    /// Its id before it was taken.
    pub fn id(&self) -> i64 {
        self.clip.id
    }
}

fn copy_kind(content: &ClipContent) -> CopyKind {
    match content {
        ClipContent::Image(_) => CopyKind::Image,
        ClipContent::Text(t) if crate::model::link_of(t).is_some() => CopyKind::Link,
        ClipContent::Text(_) => CopyKind::Text,
    }
}

/// Whether content is worth a clip: not empty text, not over `MAX_CLIP_BYTES`.
fn keeps(content: &ClipContent) -> bool {
    match content {
        ClipContent::Text(s) => s.len() <= MAX_CLIP_BYTES && !s.trim().is_empty(),
        ClipContent::Image(png) => png.len() <= MAX_CLIP_BYTES,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(1000, 500, image::Rgba([10, 20, 30, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn every_copy_is_counted_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path()).unwrap();
        assert_eq!(h.copy_total().unwrap(), (0, None));
        h.add(&ClipContent::Text("hello".into()), None).unwrap();
        h.add(&ClipContent::Text("hello".into()), None).unwrap(); // a repeat counts too
        h.add(&ClipContent::Text("https://pastezo.app".into()), None).unwrap();
        h.add(&ClipContent::Text("   ".into()), None).unwrap(); // not kept, not counted
        // copied from our own window: no new clip, still a copy
        let own = ClipContent::Text("from the window".into());
        h.mark_own_write(&own).unwrap();
        assert!(h.add(&own, None).unwrap().is_none());

        let (total, since) = h.copy_total().unwrap();
        assert_eq!(total, 4);
        assert!(since.is_some());
        let stats = h.copy_stats(0).unwrap();
        let of = |k: CopyKind| stats.iter().filter(|c| c.kind == k).map(|c| c.count).sum::<u32>();
        assert_eq!((of(CopyKind::Text), of(CopyKind::Link), of(CopyKind::Image)), (3, 1, 0));
        // an import is not a copy
        h.import(&ClipContent::Text("old".into()), None, 1, false).unwrap();
        assert_eq!(h.copy_total().unwrap().0, 4);
        // "Clear all" keeps the counts (no content in them)
        h.clear().unwrap();
        assert_eq!(h.copy_total().unwrap().0, 4);
        assert!(h.copy_stats(i64::MAX / 2).unwrap().is_empty());
    }

    #[test]
    fn clips_over_the_limit_are_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path()).unwrap();
        let big = "x".repeat(MAX_CLIP_BYTES + 1);
        assert!(h.add(&ClipContent::Text(big), None).unwrap().is_none());
        assert!(h.add(&ClipContent::Image(vec![0; MAX_CLIP_BYTES + 1]), None).unwrap().is_none());
        let just_fits = "y".repeat(MAX_CLIP_BYTES);
        assert!(h.add(&ClipContent::Text(just_fits), None).unwrap().is_some());
        assert_eq!(h.list(0, 10).unwrap().len(), 1);
    }

    #[test]
    fn image_saved_with_thumb_and_removed_on_delete() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path()).unwrap();
        let (clip, _) = h.add(&ClipContent::Image(png()), Some("Preview")).unwrap().unwrap();

        let thumb = image::open(clip.thumb_path.as_ref().unwrap()).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (720, 360));
        assert_eq!(h.content(&clip).unwrap(), ClipContent::Image(png()));

        h.delete(clip.id).unwrap();
        assert!(!Path::new(clip.image_path.as_ref().unwrap()).exists());
        assert!(!Path::new(clip.thumb_path.as_ref().unwrap()).exists());
    }

    #[test]
    fn preview_is_jpeg_for_opaque_and_png_for_transparent() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path()).unwrap();
        let encode = |img: image::RgbaImage| {
            let mut out = std::io::Cursor::new(Vec::new());
            img.write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        // noisy "photo": where PNG is at its worst
        let photo = image::RgbaImage::from_fn(1200, 800, |x, y| {
            let v = ((x * 7919 + y * 104729) % 251) as u8;
            image::Rgba([v, v / 2 + (x % 97) as u8, 255 - v, 255])
        });
        let (c, _) = h.add(&ClipContent::Image(encode(photo.clone())), None).unwrap().unwrap();
        let thumb = c.thumb_path.unwrap();
        assert!(thumb.ends_with("_thumb.jpg"), "{thumb}");
        let jpeg_size = fs::metadata(&thumb).unwrap().len();
        let png_size = {
            let mut out = std::io::Cursor::new(Vec::new());
            image::DynamicImage::ImageRgba8(photo).thumbnail(720, 720).write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner().len() as u64
        };
        assert!(jpeg_size * 3 < png_size, "jpeg {jpeg_size} vs png {png_size}");

        let mut logo = image::RgbaImage::from_pixel(200, 200, image::Rgba([255, 0, 0, 255]));
        logo.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
        let (c, _) = h.add(&ClipContent::Image(encode(logo)), None).unwrap().unwrap();
        assert!(c.thumb_path.unwrap().ends_with("_thumb.png"));
    }

    #[test]
    fn import_keeps_time_and_pin_and_skips_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path()).unwrap();
        h.add(&ClipContent::Text("here".into()), None).unwrap();
        assert!(!h.import(&ClipContent::Text("here".into()), Some("Notes"), 1, true).unwrap());
        assert!(!h.import(&ClipContent::Text(" ".into()), None, 1, false).unwrap());
        assert!(h.import(&ClipContent::Text("old".into()), Some("Notes"), 1_000, true).unwrap());
        assert!(h.import(&ClipContent::Image(png()), None, 2_000, false).unwrap());
        let all = h.list(0, 10).unwrap();
        let old = &all[0];
        assert!(old.pinned && old.created_at == 1_000 && old.source_app.as_deref() == Some("Notes"));
        assert!(all[2].thumb_path.is_some(), "an imported image gets its preview");
        assert_eq!(h.ids().unwrap().len(), 3);
    }

    #[test]
    fn a_taken_clip_comes_back_or_goes_for_good() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path()).unwrap();
        let (text, _) = h.add(&ClipContent::Text("long ".repeat(1000)), Some("Notes")).unwrap().unwrap();
        let (img, _) = h.add(&ClipContent::Image(png()), None).unwrap().unwrap();
        h.set_pinned(text.id, true).unwrap();

        let d = h.take(text.id).unwrap().unwrap();
        assert!(h.list(0, 10).unwrap().iter().all(|c| c.id != text.id));
        let back = h.restore(&d).unwrap();
        assert!(back.pinned && back.created_at == text.created_at && back.source_app.as_deref() == Some("Notes"));
        assert_eq!(h.content(&back).unwrap(), ClipContent::Text("long ".repeat(1000)), "the full text, not the preview");

        // an image keeps its files while it can come back, loses them after
        let d = h.take(img.id).unwrap().unwrap();
        let file = img.image_path.clone().unwrap();
        assert!(Path::new(&file).exists());
        h.restore(&d).unwrap();
        assert_eq!(h.list(0, 10).unwrap().len(), 2);
        let again = h.list(0, 10).unwrap().into_iter().find(|c| c.hash == img.hash).unwrap();
        let d = h.take(again.id).unwrap().unwrap();
        h.discard(d);
        assert!(!Path::new(&file).exists());
    }

    #[test]
    fn discarding_keeps_files_of_the_same_image_copied_again() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path()).unwrap();
        let (img, _) = h.add(&ClipContent::Image(png()), None).unwrap().unwrap();
        let d = h.take(img.id).unwrap().unwrap();
        h.add(&ClipContent::Image(png()), None).unwrap();
        h.discard(d);
        assert!(Path::new(img.image_path.as_ref().unwrap()).exists());
    }

    #[test]
    fn old_clips_are_forgotten_but_pinned_ones_stay() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path()).unwrap();
        let now = 100 * DAY_MS;
        let at = |days_ago: i64| now - days_ago * DAY_MS;
        h.import(&ClipContent::Text("fresh".into()), None, at(3), false).unwrap();
        h.import(&ClipContent::Text("old".into()), None, at(10), false).unwrap();
        h.import(&ClipContent::Text("old pinned".into()), None, at(10), true).unwrap();
        h.import(&ClipContent::Image(png()), None, at(40), false).unwrap();
        let image = h.list(0, 10).unwrap().into_iter().find(|c| c.image_path.is_some()).unwrap();

        // for ever by default: nothing goes
        assert_eq!(h.keep_days(), None);
        assert_eq!(h.forget_old(now).unwrap(), 0);

        h.set_keep_days(Some(30)).unwrap();
        assert_eq!(h.keep_days(), Some(30));
        assert_eq!(h.forget_old(now).unwrap(), 1);
        assert!(!Path::new(image.image_path.as_ref().unwrap()).exists(), "its files go too");
        assert!(!Path::new(image.thumb_path.as_ref().unwrap()).exists());

        h.set_keep_days(Some(7)).unwrap();
        assert_eq!(h.forget_old(now).unwrap(), 1);
        let left: Vec<_> = h.list(0, 10).unwrap().into_iter().map(|c| c.preview.unwrap()).collect();
        assert_eq!(left, ["old pinned", "fresh"]);

        h.set_keep_days(None).unwrap();
        h.set_keep_days(None).unwrap(); // no file: still fine
        assert_eq!(h.keep_days(), None);
        fs::write(dir.path().join(KEEP_DAYS_FILE), "soon").unwrap();
        assert_eq!(h.keep_days(), None, "a broken file keeps everything");
    }

    #[test]
    fn empty_text_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path()).unwrap();
        assert!(h.add(&ClipContent::Text("  \n".into()), None).unwrap().is_none());
        assert!(h.list(0, 10).unwrap().is_empty());
    }
}
