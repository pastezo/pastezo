//! Conversion between PNG and the Windows clipboard bitmap format (CF_DIB).
//! Platform-independent so it is tested on every OS.

use std::io::Cursor;

use image::codecs::bmp::BmpDecoder;
use image::{DynamicImage, ImageFormat};

use crate::Result;

/// CF_DIB (BITMAPINFO + pixels, no file header) -> PNG.
pub(crate) fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    let decoder = BmpDecoder::new_without_file_header(Cursor::new(dib)).ok()?;
    let mut rgba = DynamicImage::from_decoder(decoder).ok()?.into_rgba8();
    // Many apps leave the alpha byte of 32-bit DIBs at 0 although the image is opaque.
    if rgba.pixels().all(|p| p[3] == 0) {
        rgba.pixels_mut().for_each(|p| p[3] = 255);
    }
    let mut out = Cursor::new(Vec::new());
    rgba.write_to(&mut out, ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// PNG -> CF_DIB: BITMAPINFOHEADER + 32-bit BGRA rows, bottom-up.
pub(crate) fn png_to_dib(png: &[u8]) -> Result<Vec<u8>> {
    let img = image::load_from_memory_with_format(png, ImageFormat::Png)?.into_rgba8();
    let (w, h) = img.dimensions();
    let mut dib = Vec::with_capacity(40 + (w * h * 4) as usize);
    dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
    dib.extend_from_slice(&(w as i32).to_le_bytes()); // biWidth
    dib.extend_from_slice(&(h as i32).to_le_bytes()); // biHeight > 0: bottom-up
    dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    dib.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    dib.extend_from_slice(&(w * h * 4).to_le_bytes()); // biSizeImage
    dib.extend_from_slice(&[0u8; 16]); // resolution, colors used/important
    for row in img.rows().rev() {
        for p in row {
            dib.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    Ok(dib)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dib_roundtrip_keeps_pixels() {
        let img = image::RgbaImage::from_fn(3, 2, |x, y| image::Rgba([x as u8 * 80, y as u8 * 120, 7, 255]));
        let mut png = Cursor::new(Vec::new());
        img.write_to(&mut png, ImageFormat::Png).unwrap();
        let dib = png_to_dib(png.get_ref()).unwrap();
        let back = image::load_from_memory(&dib_to_png(&dib).unwrap()).unwrap().into_rgba8();
        assert_eq!(back, img);
    }

    /// 32-bit DIB with alpha left at 0 (what GDI screenshots look like) must come out opaque.
    #[test]
    fn zero_alpha_dib_becomes_opaque() {
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&[0u8; 24]);
        dib.extend_from_slice(&[10, 20, 30, 0]); // B G R A
        let img = image::load_from_memory(&dib_to_png(&dib).unwrap()).unwrap().into_rgba8();
        assert_eq!(img.get_pixel(0, 0).0, [30, 20, 10, 255]);
    }
}
