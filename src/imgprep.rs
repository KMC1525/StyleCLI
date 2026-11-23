//! Prepara cada imagem para o modelo: decodifica (qualquer formato suportado),
//! reduz para um orçamento de pixels (pensado para PCs fracos) e reencoda em JPEG.
//! O llama.cpp não lê WebP, então sempre reencodar também garante compatibilidade.

use anyhow::{Context, Result};
use image::{DynamicImage, ImageReader, Limits, RgbImage, imageops::FilterType};
use std::io::Cursor;

const MIN_SIDE: u32 = 32;

pub struct Prepared {
    pub jpeg: Vec<u8>,
}

/// Dimensões de saída: mantém a proporção e limita a área a `budget` pixels.
pub fn target_size(w: u32, h: u32, budget: u32) -> (u32, u32) {
    let (mut nw, mut nh) = (w as f64, h as f64);
    let area = nw * nh;
    if area > budget as f64 {
        let k = (budget as f64 / area).sqrt();
        nw *= k;
        nh *= k;
    }
    let small = nw.min(nh);
    if small < MIN_SIDE as f64 {
        let k = MIN_SIDE as f64 / small;
        nw *= k;
        nh *= k;
    }
    (
        (nw.round() as u32).max(MIN_SIDE),
        (nh.round() as u32).max(MIN_SIDE),
    )
}

pub fn prepare(data: &[u8], image_size: u32) -> Result<Prepared> {
    let mut reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .context("formato de imagem não reconhecido")?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(768 * 1024 * 1024);
    reader.limits(limits);
    let img = reader
        .decode()
        .context("imagem corrompida ou não suportada")?;
    let (w, h) = (img.width(), img.height());

    let (nw, nh) = target_size(w, h, image_size.saturating_mul(image_size));
    let rgb = flatten_on_white(&img);
    let rgb = if (nw, nh) != (w, h) {
        image::imageops::resize(&rgb, nw, nh, FilterType::Triangle)
    } else {
        rgb
    };

    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 88)
        .encode_image(&rgb)
        .context("reencodando imagem")?;
    Ok(Prepared { jpeg })
}

/// Compõe transparência sobre fundo branco (JPEG não tem alfa).
fn flatten_on_white(img: &DynamicImage) -> RgbImage {
    if !img.color().has_alpha() {
        return img.to_rgb8();
    }
    let rgba = img.to_rgba8();
    let mut out = RgbImage::new(rgba.width(), rgba.height());
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = p[3] as u32;
        let blend = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
        out.put_pixel(x, y, image::Rgb([blend(p[0]), blend(p[1]), blend(p[2])]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(target_size(100, 100, 640 * 640), (100, 100));
        let (w, h) = target_size(4000, 3000, 640 * 640);
        assert!(w * h <= 640 * 640 + 2000 && w > h);
        // tira de webtoon: continua legível, nunca abaixo do mínimo
        let (w, h) = target_size(800, 12000, 640 * 640);
        assert!(w >= MIN_SIDE && h > w * 10);
        assert_eq!(target_size(10, 10, 1000), (32, 32));
    }

    #[test]
    fn png_with_alpha_to_jpeg() {
        let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([255, 0, 0, 0]));
        let mut png = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let p = prepare(&png, 640).unwrap();
        assert_eq!(&p.jpeg[..2], &[0xFF, 0xD8]);
        let back = image::load_from_memory(&p.jpeg).unwrap().to_rgb8();
        assert!(back.get_pixel(10, 10)[1] > 240); // transparente virou branco
    }

    #[test]
    fn garbage_is_error() {
        assert!(prepare(b"isto nao e imagem", 640).is_err());
    }
}
