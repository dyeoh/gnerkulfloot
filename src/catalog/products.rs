//! Products, as staff manage them. Everything here sees drafts and archived
//! products too; the storefront view lives in [`super::storefront`].

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgConnection, PgPool, Postgres, QueryBuilder};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    CatalogError,
    images::{self, ImageView},
    map_unique, require_name,
    skus::{self, Sku},
    slugify, validate_slug,
};
use crate::storage::Storage;

/// Only `active` products are visible to shoppers. Products are archived
/// rather than deleted, so past orders can keep pointing at them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum ProductStatus {
    Draft,
    Active,
    Archived,
}

#[derive(Debug, Serialize)]
pub struct Product {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub status: ProductStatus,
    pub attributes: Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// A product with everything staff need to edit it.
#[derive(Debug, Serialize)]
pub struct ProductDetail {
    #[serde(flatten)]
    pub product: Product,
    pub category_ids: Vec<Uuid>,
    pub skus: Vec<Sku>,
    pub images: Vec<ImageView>,
}

#[derive(Deserialize)]
pub struct NewProduct {
    pub name: String,
    pub slug: Option<String>,
    #[serde(default)]
    pub description: String,
    pub status: Option<ProductStatus>,
    pub attributes: Option<Value>,
    #[serde(default)]
    pub category_ids: Vec<Uuid>,
}

#[derive(Deserialize)]
pub struct ProductPatch {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub status: Option<ProductStatus>,
    pub attributes: Option<Value>,
    /// Replaces the product's categories when present.
    pub category_ids: Option<Vec<Uuid>>,
}

fn validate_attributes(attributes: &Option<Value>) -> Result<(), CatalogError> {
    match attributes {
        Some(v) if !v.is_object() => Err(CatalogError::InvalidInput("attributes must be a JSON object".into())),
        _ => Ok(()),
    }
}

pub async fn create(db: &PgPool, storage: &Storage, new: NewProduct) -> Result<ProductDetail, CatalogError> {
    let name = require_name(&new.name)?;
    let slug = new.slug.unwrap_or_else(|| slugify(&name));
    validate_slug(&slug)?;
    validate_attributes(&new.attributes)?;

    let mut tx = db.begin().await?;
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO products (id, slug, name, description, status, attributes)
         VALUES ($1, $2, $3, $4, $5, $6)",
        id,
        slug,
        name,
        new.description,
        new.status.unwrap_or(ProductStatus::Draft) as ProductStatus,
        new.attributes.unwrap_or_else(|| Value::Object(Default::default())),
    )
    .execute(&mut *tx)
    .await
    .map_err(map_unique)?;
    set_categories(&mut tx, id, &new.category_ids).await?;
    tx.commit().await?;
    get(db, storage, id).await
}

pub async fn update(
    db: &PgPool,
    storage: &Storage,
    id: Uuid,
    patch: ProductPatch,
) -> Result<ProductDetail, CatalogError> {
    let name = patch.name.as_deref().map(require_name).transpose()?;
    if let Some(slug) = &patch.slug {
        validate_slug(slug)?;
    }
    validate_attributes(&patch.attributes)?;

    let mut tx = db.begin().await?;
    let updated = sqlx::query!(
        "UPDATE products SET
             name        = COALESCE($2, name),
             slug        = COALESCE($3, slug),
             description = COALESCE($4, description),
             status      = COALESCE($5, status),
             attributes  = COALESCE($6, attributes),
             updated_at  = now()
         WHERE id = $1",
        id,
        name,
        patch.slug,
        patch.description,
        patch.status as Option<ProductStatus>,
        patch.attributes,
    )
    .execute(&mut *tx)
    .await
    .map_err(map_unique)?;
    if updated.rows_affected() == 0 {
        return Err(CatalogError::NotFound);
    }
    if let Some(ids) = &patch.category_ids {
        set_categories(&mut tx, id, ids).await?;
    }
    tx.commit().await?;
    get(db, storage, id).await
}

async fn set_categories(conn: &mut PgConnection, product_id: Uuid, category_ids: &[Uuid]) -> Result<(), CatalogError> {
    sqlx::query!("DELETE FROM product_categories WHERE product_id = $1", product_id)
        .execute(&mut *conn)
        .await?;
    let inserted = sqlx::query!(
        "INSERT INTO product_categories (product_id, category_id)
         SELECT $1, id FROM categories WHERE id = ANY($2)",
        product_id,
        category_ids
    )
    .execute(&mut *conn)
    .await?;
    let distinct = category_ids.iter().collect::<std::collections::HashSet<_>>().len() as u64;
    if inserted.rows_affected() != distinct {
        return Err(CatalogError::InvalidInput(
            "one or more category_ids don't exist".into(),
        ));
    }
    Ok(())
}

pub async fn get(db: &PgPool, storage: &Storage, id: Uuid) -> Result<ProductDetail, CatalogError> {
    let product = sqlx::query_as!(
        Product,
        r#"SELECT id, slug, name, description, status AS "status: ProductStatus", attributes, created_at, updated_at
           FROM products WHERE id = $1"#,
        id
    )
    .fetch_optional(db)
    .await?
    .ok_or(CatalogError::NotFound)?;
    let category_ids = sqlx::query_scalar!("SELECT category_id FROM product_categories WHERE product_id = $1", id)
        .fetch_all(db)
        .await?;
    Ok(ProductDetail {
        product,
        category_ids,
        skus: skus::for_product(db, id).await?,
        images: images::for_product(db, storage, id).await?,
    })
}

/// Filters for the admin product list.
#[derive(Deserialize, Default)]
pub struct AdminListQuery {
    pub status: Option<ProductStatus>,
    pub q: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct AdminListItem {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub status: ProductStatus,
    pub sku_count: i64,
    pub total_stock: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// Admin product list, newest-edited first. Built dynamically because every
/// filter is optional.
pub async fn list(db: &PgPool, q: AdminListQuery) -> Result<super::storefront::Page<AdminListItem>, sqlx::Error> {
    let (page, per_page) = super::storefront::paging(q.page, q.per_page);
    let mut sql = QueryBuilder::<Postgres>::new(
        "SELECT p.id, p.slug, p.name, p.status, p.updated_at,
                COUNT(s.id) AS sku_count, COALESCE(SUM(s.stock_available), 0)::bigint AS total_stock
         FROM products p LEFT JOIN skus s ON s.product_id = p.id WHERE true",
    );
    if let Some(status) = q.status {
        sql.push(" AND p.status = ").push_bind(status);
    }
    if let Some(term) = q.q.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        sql.push(" AND (p.name ILIKE ")
            .push_bind(super::storefront::like_pattern(term))
            .push(" OR p.slug ILIKE ")
            .push_bind(super::storefront::like_pattern(term))
            .push(" OR EXISTS (SELECT 1 FROM skus s2 WHERE s2.product_id = p.id AND s2.code ILIKE ")
            .push_bind(super::storefront::like_pattern(term))
            .push("))");
    }
    sql.push(" GROUP BY p.id ORDER BY p.updated_at DESC, p.id LIMIT ")
        .push_bind(i64::from(per_page) + 1)
        .push(" OFFSET ")
        .push_bind(i64::from((page - 1) * per_page));
    let rows = sql.build_query_as::<AdminListItem>().fetch_all(db).await?;
    Ok(super::storefront::Page::new(rows, page, per_page))
}
