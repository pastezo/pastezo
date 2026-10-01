//! Site icons before links in the list. An icon comes from the site itself,
//! `<scheme>://<host>[:port]/favicon.ico` (the rest of the link never leaves
//! the computer), once, in the background, and is kept in the data folder as
//! a small PNG: from then on it is read from there. A site that answered
//! without an icon keeps the link icon for good (an empty file says so); no
//! network: asked again the next time the window opens.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use image::imageops::FilterType;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use crate::{net, thumbs};

/// Box an icon is drawn in, logical pixels (`ClipRow` in ui/app.slint).
const SIZE: f32 = 16.0;
/// Pixels an icon is kept at: sharp at `SIZE` on up to 4× screens.
const KEEP: u32 = 64;
/// A favicon.ico bigger than this is not an icon.
const LIMIT: usize = 512 * 1024;
/// Sites asked at the same time: one that never answers holds up only one.
const WORKERS: usize = 4;
/// Decoded icons kept around.
const CACHE: usize = 64;

thread_local! {
    static DECODED: thumbs::Cache = const { RefCell::new(VecDeque::new()) };
}

/// Gets a URL (swapped out in tests).
pub type Download = fn(&str) -> Result<Vec<u8>, net::Error>;

/// The icon file from the network.
pub fn download(url: &str) -> Result<Vec<u8>, net::Error> {
    net::get(url, "image/*", LIMIT)
}

pub struct Favicons {
    /// `<data>/favicons`: `<site>.png`, an empty one for a site with none
    dir: PathBuf,
    download: Download,
    /// sites asked for since the window opened (each once)
    asked: RefCell<HashSet<String>>,
    /// sites waiting for a worker, and how many workers run
    queue: Arc<Mutex<(VecDeque<String>, usize)>>,
    /// a worker saved an icon: the list should show it
    fetched: Arc<AtomicBool>,
}

impl Favicons {
    pub fn new(dir: PathBuf, download: Download) -> Self {
        Favicons { dir, download, asked: RefCell::default(), queue: Arc::default(), fetched: Arc::default() }
    }

    /// The saved icon of the link's site ("": none yet). The first time,
    /// asks the site for one in the background.
    pub fn path(&self, link: &str) -> String {
        let Some(origin) = origin(link) else { return String::new() };
        let file = self.dir.join(file_name(&origin));
        match std::fs::metadata(&file) {
            Ok(m) if m.len() > 0 => return file.to_string_lossy().into_owned(),
            // asked before, the site had none
            Ok(_) => return String::new(),
            Err(_) => {}
        }
        if self.asked.borrow_mut().insert(origin.clone()) {
            self.ask(origin);
        }
        String::new()
    }

    /// An icon came in since the last call.
    pub fn take_fetched(&self) -> bool {
        self.fetched.swap(false, Ordering::Relaxed)
    }

    /// Clear history: the sites it had go too.
    pub fn clear(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        self.asked.borrow_mut().clear();
    }

    fn ask(&self, origin: String) {
        let mut queue = self.queue.lock().unwrap();
        queue.0.push_back(origin);
        if queue.1 == WORKERS {
            return;
        }
        queue.1 += 1;
        let (queue, fetched, dir, download) = (self.queue.clone(), self.fetched.clone(), self.dir.clone(), self.download);
        // a worker takes sites until none are left, then ends
        std::thread::spawn(move || loop {
            let next = {
                let mut q = queue.lock().unwrap();
                let next = q.0.pop_front();
                if next.is_none() {
                    q.1 -= 1;
                }
                next
            };
            let Some(origin) = next else { return };
            if fetch(&dir, &origin, download) {
                fetched.store(true, Ordering::Relaxed);
            }
        });
    }
}

/// Asks the site for its icon and saves it (an empty file: the site has
/// none). Whether there was one.
fn fetch(dir: &Path, origin: &str, download: Download) -> bool {
    let png = match download(&format!("{origin}/favicon.ico")) {
        Ok(bytes) => to_png(&bytes),
        Err(net::Error::Refused) => None,
        // nothing is known yet: nothing saved
        Err(net::Error::Unreachable) => return false,
    };
    let file = dir.join(file_name(origin));
    // written next to it, then renamed: the list never reads half a file
    let part = file.with_extension("part");
    let saved = std::fs::create_dir_all(dir)
        .and_then(|_| std::fs::write(&part, png.as_deref().unwrap_or_default()))
        .and_then(|_| std::fs::rename(&part, &file));
    saved.is_ok() && png.is_some()
}

/// The icon as a PNG of at most `KEEP` px; `None`: not an image the app
/// reads (a web page, an SVG).
fn to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width() > KEEP || img.height() > KEEP { img.resize(KEEP, KEEP, FilterType::Triangle) } else { img };
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(png)
}

/// "https://github.com" of "https://github.com/a?b" (with the port, when not
/// the usual one).
fn origin(link: &str) -> Option<String> {
    let url = url::Url::parse(link).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.origin().ascii_serialization())
}

/// "github.com.png", "localhost_3000.png": the site without the scheme, a
/// file name on every OS.
fn file_name(origin: &str) -> String {
    let site = origin.split_once("://").map_or(origin, |(_, site)| site);
    let site: String = site.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') { c } else { '_' }).collect();
    format!("{site}.png")
}

/// A saved icon, decoded at `scale_factor` × its box.
pub fn load(path: &str, scale_factor: f32) -> Image {
    if path.is_empty() {
        return Image::default();
    }
    thumbs::cached(&DECODED, CACHE, path, || {
        let side = (SIZE * scale_factor).round() as u32;
        let img = image::open(path).ok()?.resize(side, side, FilterType::Triangle).into_rgba8();
        Some(Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(img.as_raw(), img.width(), img.height())))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    fn png(side: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(side, side, image::Rgba([200, 30, 30, 255]));
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        png
    }

    /// Waits for the workers: whether an icon came in.
    fn settled(f: &Favicons) -> bool {
        for _ in 0..500 {
            let q = f.queue.lock().unwrap();
            if q.0.is_empty() && q.1 == 0 {
                return f.take_fetched();
            }
            drop(q);
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the workers did not finish");
    }

    #[test]
    fn origins_and_file_names() {
        assert_eq!(origin("https://github.com/a/b?c=1#d").as_deref(), Some("https://github.com"));
        assert_eq!(origin("http://localhost:3000/x").as_deref(), Some("http://localhost:3000"));
        assert_eq!(origin("https://example.com:443/").as_deref(), Some("https://example.com"));
        assert_eq!(origin("file:///etc/passwd"), None);
        assert_eq!(file_name("https://github.com"), "github.com.png");
        assert_eq!(file_name("http://localhost:3000"), "localhost_3000.png");
        assert_eq!(file_name("http://[::1]:8080"), "___1__8080.png");
    }

    #[test]
    fn an_icon_is_fetched_once_and_kept_small() {
        static ASKED: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let dir = tempfile::tempdir().unwrap();
        let f = Favicons::new(dir.path().join("favicons"), |url| {
            ASKED.lock().unwrap().push(url.into());
            Ok(png(256))
        });
        assert_eq!(f.path("https://example.com/secret?token=1"), "", "not there yet");
        assert_eq!(f.path("https://example.com/other"), "", "asked for already");
        assert!(settled(&f));
        assert_eq!(*ASKED.lock().unwrap(), ["https://example.com/favicon.ico"], "only the site, once");
        let path = f.path("https://example.com/");
        assert!(path.ends_with("example.com.png"), "{path}");
        assert_eq!(image::image_dimensions(&path).unwrap(), (KEEP, KEEP));
        f.clear();
        assert!(!dir.path().join("favicons").exists());
    }

    #[test]
    fn a_site_without_an_icon_is_not_asked_again() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let dir = tempfile::tempdir().unwrap();
        let download: Download = |url| {
            CALLS.fetch_add(1, Ordering::Relaxed);
            match url {
                // a page where the icon should be
                "https://example.org/favicon.ico" => Ok(b"<!doctype html><title>Not found</title>".to_vec()),
                _ => Err(net::Error::Refused),
            }
        };
        let f = Favicons::new(dir.path().to_path_buf(), download);
        assert_eq!(f.path("https://example.org/"), "");
        assert_eq!(f.path("https://example.net/"), "");
        assert!(!settled(&f));
        for site in ["example.org", "example.net"] {
            assert_eq!(std::fs::metadata(dir.path().join(format!("{site}.png"))).unwrap().len(), 0, "{site}");
        }
        // the window opened again: the empty files say "none", no new requests
        let again = Favicons::new(dir.path().to_path_buf(), download);
        assert_eq!(again.path("https://example.org/"), "");
        assert_eq!(again.path("https://example.net/"), "");
        assert!(!settled(&again));
        assert_eq!(CALLS.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn no_network_is_asked_again_next_time() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let dir = tempfile::tempdir().unwrap();
        let download: Download = |_| {
            CALLS.fetch_add(1, Ordering::Relaxed);
            Err(net::Error::Unreachable)
        };
        let f = Favicons::new(dir.path().to_path_buf(), download);
        assert_eq!(f.path("https://example.com/"), "");
        assert!(!settled(&f));
        assert!(!dir.path().join("example.com.png").exists(), "nothing known about the site");
        assert_eq!(f.path("https://example.com/"), "", "not again while the window is open");
        assert!(!settled(&f));
        assert_eq!(CALLS.load(Ordering::Relaxed), 1);
        let again = Favicons::new(dir.path().to_path_buf(), download);
        assert_eq!(again.path("https://example.com/"), "");
        assert!(!settled(&again));
        assert_eq!(CALLS.load(Ordering::Relaxed), 2);
    }
}
