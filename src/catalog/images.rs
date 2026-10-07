//! Product images: validation, resizing and storage.
//!
//! Uploads are never stored as sent. We check the real file type from its
//! bytes, decode with size limits (so a tiny file can't claim to be a
//! 100,000 × 100,000 image and exhaust memory), rotate phone photos upright,
//! and re-encode to WebP. Re-encoding also strips metadata such as the GPS
//! location phones embed in photos.

use std::io::Cursor;

use bytes::Bytes;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, imageops::FilterType};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::CatalogError;
use crate::storage::Storage;

/// Sizes we generate: (name, longest side in pixels). Smaller images are never upscaled.
const VARIANTS: [(&str, u32); 2] = [("large", 1600), ("thumb", 400)];
const WEBP_QUALITY: f32 = 82.0;
const MAX_DIMENSION: u32 = 12_000;
const MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;

/// The result of processing one upload, ready to store.
pub struct Processed {
    pub key_prefix: String,
    pub width: u32,
    pub height: u32,
    pub files: Vec<(String, Bytes)>,
}

/// Validates, normalizes and resizes an uploaded image. CPU-heavy: call from
/// a blocking thread.
pub fn process(upload: &[u8]) -> Result<Processed, CatalogError> {
    let unsupported = || CatalogError::UnsupportedImage("upload a JPEG, PNG, WebP or GIF image".into());
    let format = image::guess_format(upload).map_err(|_| unsupported())?;
    if !matches!(
        format,
        ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP | ImageFormat::Gif
    ) {
        return Err(unsupported());
    }

    let mut reader = ImageReader::with_format(Cursor::new(upload), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let unreadable = |e: image::ImageError| CatalogError::InvalidInput(format!("couldn't read image: {e}"));
    let mut decoder = reader.into_decoder().map_err(unreadable)?;
    let orientation = decoder.orientation().map_err(unreadable)?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(unreadable)?;
    img.apply_orientation(orientation);

    // Same bytes in → same key out, so re-uploading an image stores nothing new.
    let digest = Sha256::digest(upload);
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    let key_prefix = format!("images/{hex}");

    let files = VARIANTS
        .iter()
        .map(|(name, max)| {
            let sized = if img.width() <= *max && img.height() <= *max {
                img.clone()
            } else {
                img.resize(*max, *max, FilterType::Lanczos3)
            };
            (format!("{key_prefix}/{name}.webp"), encode_webp(&sized))
        })
        .collect();

    Ok(Processed {
        key_prefix,
        width: img.width(),
        height: img.height(),
        files,
    })
}

fn encode_webp(img: &DynamicImage) -> Bytes {
    let (w, h) = (img.width(), img.height());
    // Keep an alpha channel only when the image has one; RGB files are smaller.
    let encoded = if img.color().has_alpha() {
        let rgba = img.to_rgba8();
        webp::Encoder::from_rgba(&rgba, w, h).encode(WEBP_QUALITY)
    } else {
        let rgb = img.to_rgb8();
        webp::Encoder::from_rgb(&rgb, w, h).encode(WEBP_QUALITY)
    };
    Bytes::copy_from_slice(&encoded)
}

/// An image as shown to API clients, with ready-to-use URLs.
#[derive(Debug, Serialize)]
pub struct ImageView {
    pub id: Uuid,
    pub sku_id: Option<Uuid>,
    pub alt: String,
    pub width: i32,
    pub height: i32,
    pub position: i32,
    pub urls: ImageUrls,
}

#[derive(Debug, Serialize)]
pub struct ImageUrls {
    pub large: String,
    pub thumb: String,
}

pub(crate) fn urls(storage: &Storage, key_prefix: &str) -> ImageUrls {
    ImageUrls {
        large: storage.url(&format!("{key_prefix}/large.webp")),
        thumb: storage.url(&format!("{key_prefix}/thumb.webp")),
    }
}

/// Images for one product, in display order.
pub async fn for_product(db: &PgPool, storage: &Storage, product_id: Uuid) -> Result<Vec<ImageView>, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT id, sku_id, key_prefix, alt, width, height, position FROM product_images
         WHERE product_id = $1 ORDER BY position, created_at",
        product_id
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ImageView {
            id: r.id,
            sku_id: r.sku_id,
            alt: r.alt,
            width: r.width,
            height: r.height,
            position: r.position,
            urls: urls(storage, &r.key_prefix),
        })
        .collect())
}

/// Processes an upload, stores its files and attaches it to a product
/// (and optionally to one of its SKUs), at the end of the image list.
pub async fn upload(
    db: &PgPool,
    storage: &Storage,
    product_id: Uuid,
    sku_id: Option<Uuid>,
    alt: String,
    bytes: Bytes,
) -> Result<ImageView, CatalogError> {
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM products WHERE id = $1) AS "exists!""#,
        product_id
    )
    .fetch_one(db)
    .await?;
    if !exists {
        return Err(CatalogError::NotFound);
    }
    if let Some(sku_id) = sku_id {
        let belongs = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM skus WHERE id = $1 AND product_id = $2) AS "belongs!""#,
            sku_id,
            product_id
        )
        .fetch_one(db)
        .await?;
        if !belongs {
            return Err(CatalogError::InvalidInput(
                "sku_id is not a variant of this product".into(),
            ));
        }
    }

    let processed = tokio::task::spawn_blocking(move || process(&bytes))
        .await
        .map_err(|e| CatalogError::InvalidInput(format!("image processing failed: {e}")))??;
    for (key, data) in processed.files {
        storage.put(&key, data, "image/webp").await?;
    }

    let row = sqlx::query!(
        "INSERT INTO product_images (id, product_id, sku_id, key_prefix, width, height, alt, position)
         VALUES ($1, $2, $3, $4, $5, $6, $7,
                 (SELECT COALESCE(MAX(position) + 1, 0) FROM product_images WHERE product_id = $2))
         RETURNING id, position",
        Uuid::now_v7(),
        product_id,
        sku_id,
        processed.key_prefix,
        processed.width as i32,
        processed.height as i32,
        alt,
    )
    .fetch_one(db)
    .await?;

    Ok(ImageView {
        id: row.id,
        sku_id,
        alt,
        width: processed.width as i32,
        height: processed.height as i32,
        position: row.position,
        urls: urls(storage, &processed.key_prefix),
    })
}

/// Detaches an image and deletes its files, unless another product still uses
/// the same picture (uploads are content-addressed, so they can share files).
pub async fn delete(db: &PgPool, storage: &Storage, image_id: Uuid) -> Result<(), CatalogError> {
    let prefix = sqlx::query_scalar!(
        "DELETE FROM product_images WHERE id = $1 RETURNING key_prefix",
        image_id
    )
    .fetch_optional(db)
    .await?
    .ok_or(CatalogError::NotFound)?;
    let still_used = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM product_images WHERE key_prefix = $1) AS "used!""#,
        prefix
    )
    .fetch_one(db)
    .await?;
    if !still_used {
        for (name, _) in VARIANTS {
            storage.delete(&format!("{prefix}/{name}.webp")).await?;
        }
    }
    Ok(())
}

/// Updates an image's alt text and/or position.
pub async fn update(
    db: &PgPool,
    image_id: Uuid,
    alt: Option<String>,
    position: Option<i32>,
) -> Result<(), CatalogError> {
    let updated = sqlx::query!(
        "UPDATE product_images SET alt = COALESCE($2, alt), position = COALESCE($3, position) WHERE id = $1",
        image_id,
        alt,
        position
    )
    .execute(db)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(CatalogError::NotFound);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    fn png(img: DynamicImage) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn resizes_large_images_without_upscaling_small_ones() {
        let big = png(DynamicImage::ImageRgb8(RgbImage::from_pixel(
            3200,
            1600,
            Rgb([200, 30, 30]),
        )));
        let p = process(&big).unwrap();
        assert_eq!((p.width, p.height), (3200, 1600));
        let large = image::load_from_memory(&p.files[0].1).unwrap();
        let thumb = image::load_from_memory(&p.files[1].1).unwrap();
        assert_eq!((large.width(), large.height()), (1600, 800));
        assert_eq!((thumb.width(), thumb.height()), (400, 200));

        let small = png(DynamicImage::ImageRgb8(RgbImage::from_pixel(300, 200, Rgb([0, 0, 0]))));
        let p = process(&small).unwrap();
        let large = image::load_from_memory(&p.files[0].1).unwrap();
        assert_eq!((large.width(), large.height()), (300, 200));
    }

    #[test]
    fn output_is_webp_and_content_addressed() {
        let bytes = png(DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            50,
            50,
            Rgba([0, 0, 0, 0]),
        )));
        let a = process(&bytes).unwrap();
        let b = process(&bytes).unwrap();
        assert_eq!(a.key_prefix, b.key_prefix);
        assert!(a.files[0].0.ends_with("/large.webp"));
        assert_eq!(image::guess_format(&a.files[0].1).unwrap(), ImageFormat::WebP);
        // Transparency survives.
        assert!(image::load_from_memory(&a.files[0].1).unwrap().color().has_alpha());
    }

    #[test]
    fn rejects_non_images_by_content_not_name() {
        assert!(matches!(
            process(b"%PDF-1.7 not an image"),
            Err(CatalogError::UnsupportedImage(_))
        ));
        assert!(matches!(process(b"just text"), Err(CatalogError::UnsupportedImage(_))));
        // Right magic bytes, broken body.
        let mut broken = png(DynamicImage::ImageRgb8(RgbImage::new(10, 10)));
        broken.truncate(30);
        assert!(process(&broken).is_err());
    }
}
