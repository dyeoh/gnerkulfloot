//! The product catalog: categories, products, SKUs (buyable variants) with
//! per-currency prices, stock levels and images.
//!
//! Admin-side functions take what staff send and validate it; storefront-side
//! functions in [`storefront`] only ever show active products in one currency.

pub mod categories;
pub mod images;
pub mod products;
pub mod skus;
pub mod storefront;

use serde::{Deserialize, Deserializer};

use crate::error::AppError;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    InvalidInput(String),
    #[error("that slug is already used")]
    SlugTaken,
    #[error("that SKU code is already used")]
    SkuCodeTaken,
    #[error("not enough stock: {available} available")]
    InsufficientStock { available: i32 },
    #[error("{0}")]
    UnsupportedImage(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error("storage error: {0}")]
    Storage(#[from] object_store::Error),
}

impl From<CatalogError> for AppError {
    fn from(err: CatalogError) -> Self {
        match err {
            CatalogError::NotFound => AppError::NotFound,
            CatalogError::InvalidInput(msg) => AppError::BadRequest(msg),
            CatalogError::SlugTaken | CatalogError::SkuCodeTaken | CatalogError::InsufficientStock { .. } => {
                AppError::Conflict(err.to_string())
            }
            CatalogError::UnsupportedImage(msg) => AppError::UnsupportedMediaType(msg),
            CatalogError::Database(e) => e.into(),
            CatalogError::Storage(e) => AppError::Internal(e.into()),
        }
    }
}

/// Maps a unique-constraint violation to a friendly error, by constraint name.
pub(crate) fn map_unique(err: sqlx::Error) -> CatalogError {
    if let sqlx::Error::Database(db) = &err
        && db.is_unique_violation()
    {
        match db.constraint() {
            Some("products_slug_key" | "categories_slug_key") => return CatalogError::SlugTaken,
            Some("skus_code_key") => return CatalogError::SkuCodeTaken,
            _ => {}
        }
    }
    err.into()
}

/// Turns a name into a URL slug: `"Baju Kurung (Red)"` → `"baju-kurung-red"`.
/// Names with no Latin letters or digits (e.g. Chinese) get a short random slug,
/// which staff can replace with a readable one.
pub fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        format!("item-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
    } else {
        slug.chars()
            .take(80)
            .collect::<String>()
            .trim_end_matches('-')
            .to_owned()
    }
}

/// Accepts lowercase letters, digits and single dashes between them.
pub fn validate_slug(slug: &str) -> Result<(), CatalogError> {
    let ok = !slug.is_empty()
        && slug.len() <= 100
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(CatalogError::InvalidInput(
            "slug must be lowercase letters, digits and single dashes".into(),
        ))
    }
}

pub(crate) fn require_name(name: &str) -> Result<String, CatalogError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(CatalogError::InvalidInput("name must be 1–200 characters".into()));
    }
    Ok(name.to_owned())
}

/// Lets a PATCH body tell "field missing" (leave alone) from `null` (clear it):
/// missing → `None`, `null` → `Some(None)`, value → `Some(Some(v))`.
/// Use with `#[serde(default, deserialize_with = "double_option")]`.
pub(crate) fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("Baju Kurung (Red)"), "baju-kurung-red");
        assert_eq!(slugify("  Kuih -- Lapis!! "), "kuih-lapis");
        assert!(slugify("月饼").starts_with("item-"));
        assert!(validate_slug("baju-kurung-2").is_ok());
        for bad in ["", "Baju", "baju--kurung", "-baju", "baju_kurung", "baju kurung"] {
            assert!(validate_slug(bad).is_err(), "{bad:?}");
        }
    }
}
