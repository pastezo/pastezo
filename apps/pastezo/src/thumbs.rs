//! Image previews for the list: decoded only for rows on screen, scaled down to
//! the size they are shown at, a few kept in memory.

use std::cell::RefCell;
use std::collections::VecDeque;

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

/// Box a preview is drawn in, logical pixels (as in the design).
pub const MAX_W: f32 = 180.0;
pub const MAX_H: f32 = 120.0;
/// Decoded previews kept around (a page on screen shows a handful).
const CACHE: usize = 24;

thread_local! {
    static CACHE_LRU: RefCell<VecDeque<(String, Image)>> = const { RefCell::new(VecDeque::new()) };
}

/// Logical size of the preview box for an image file (reads only the header).
pub fn display_size(path: &str) -> (f32, f32) {
    let Ok((w, h)) = image::image_dimensions(path) else {
        return (0.0, 0.0);
    };
    // fit into 180×120 keeping the aspect ratio, never upscale
    let s = (MAX_W / w as f32).min(MAX_H / h as f32).min(1.0);
    ((w as f32 * s).round(), (h as f32 * s).round())
}

/// Decoded preview at `scale_factor` × its logical box.
pub fn load(path: &str, scale_factor: f32) -> Image {
    if path.is_empty() {
        return Image::default();
    }
    if let Some(img) = CACHE_LRU.with(|c| {
        let mut c = c.borrow_mut();
        let i = c.iter().position(|(p, _)| p == path)?;
        let hit = c.remove(i)?;
        c.push_front(hit.clone());
        Some(hit.1)
    }) {
        return img;
    }
    let img = decode(path, scale_factor).unwrap_or_default();
    CACHE_LRU.with(|c| {
        let mut c = c.borrow_mut();
        c.push_front((path.to_string(), img.clone()));
        c.truncate(CACHE);
    });
    img
}

fn decode(path: &str, scale_factor: f32) -> Option<Image> {
    let (w, h) = display_size(path);
    let img = image::open(path).ok()?;
    let (tw, th) = ((w * scale_factor).round() as u32, (h * scale_factor).round() as u32);
    let rgba = if tw > 0 && th > 0 && (tw < img.width() || th < img.height()) {
        img.resize_exact(tw, th, image::imageops::FilterType::Triangle).into_rgba8()
    } else {
        img.into_rgba8()
    };
    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(rgba.as_raw(), rgba.width(), rgba.height());
    Some(Image::from_rgba8(buf))
}
