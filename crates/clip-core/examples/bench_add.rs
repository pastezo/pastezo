//! Times saving clips into a fresh history: a 4K screenshot-like image and
//! many text clips. Run: `cargo run --release -p clip-core --example bench_add`.
use std::time::Instant;

use clip_core::{ClipContent, History};

fn main() {
    let dir = std::env::temp_dir().join(format!("pastezo-bench-{}", std::process::id()));
    let history = History::open(&dir).unwrap();

    let img = image::RgbaImage::from_fn(3840, 2160, |x, y| {
        image::Rgba([(x / 15) as u8, (y / 9) as u8, ((x ^ y) & 0xff) as u8, 255])
    });
    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png).unwrap();
    let png = png.into_inner();

    let t = Instant::now();
    history.add(&ClipContent::Image(png), None).unwrap();
    println!("4K image (hash + save + preview): {:?}", t.elapsed());

    let t = Instant::now();
    for i in 0..2000 {
        history.add(&ClipContent::Text(format!("clip number {i} https://example.com/{i}")), Some("Bench")).unwrap();
    }
    println!("2000 text clips: {:?}", t.elapsed());

    let t = Instant::now();
    for _ in 0..200 {
        history.list(0, 50).unwrap();
    }
    println!("200 × list(50): {:?}", t.elapsed());

    let t = Instant::now();
    for _ in 0..200 {
        history.search(&"number 19".into(), 200).unwrap();
    }
    println!("200 × search: {:?}", t.elapsed());
    std::fs::remove_dir_all(dir).unwrap();
}
