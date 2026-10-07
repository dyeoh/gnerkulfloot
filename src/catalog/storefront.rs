//! What shoppers see: active products only, priced in one currency at a time.
//! A product with no price in the requested currency isn't listed in it, since
//! there's no automatic currency conversion.

use iso_currency::Currency;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

use super::{
    CatalogError,
    images::{self, ImageView},
};
use crate::{money::Money, storage::Storage};

const DEFAULT_PER_PAGE: u32 = 24;
const MAX_PER_PAGE: u32 = 100;

/// One page of results. `has_more` instead of a total count: counting every
/// match on each request is the slow part of pagination and storefronts
/// rarely need it.
#[derive(Debug, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub page: u32,
    pub per_page: u32,
    pub has_more: bool,
}

impl<T> Page<T> {
    /// Builds a page from a query that fetched `per_page + 1` rows; the extra row only signals `has_more`.
    pub(crate) fn new(mut items: Vec<T>, page: u32, per_page: u32) -> Self {
        let has_more = items.len() > per_page as usize;
        items.truncate(per_page as usize);
        Self {
            items,
            page,
            per_page,
            has_more,
        }
    }
}

pub(crate) fn paging(page: Option<u32>, per_page: Option<u32>) -> (u32, u32) {
    let page = page.unwrap_or(1).max(1);
    let per_page = per_page.unwrap_or(DEFAULT_PER_PAGE).clamp(1, MAX_PER_PAGE);
    (page, per_page)
}

/// `%term%` for ILIKE, with the term's own `%`, `_` and `\` escaped so they match literally.
pub(crate) fn like_pattern(term: &str) -> String {
    let escaped = term.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    format!("%{escaped}%")
}

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    /// Best match first when searching, otherwise newest first.
    #[default]
    Relevance,
    Newest,
    PriceAsc,
    PriceDesc,
    Name,
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    pub currency: Option<Currency>,
    /// Free-text search over names and descriptions; tolerant of typos.
    pub q: Option<String>,
    /// Category slug.
    pub category: Option<String>,
    #[serde(default)]
    pub sort: Sort,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct ListItem {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    /// Lowest variant price ("from RM19.90").
    pub price_from: Money,
    pub compare_at: Option<Money>,
    pub in_stock: bool,
    pub thumbnail: Option<String>,
}

#[derive(FromRow)]
struct ListRow {
    id: Uuid,
    slug: String,
    name: String,
    price_amount: i64,
    compare_at_amount: Option<i64>,
    in_stock: bool,
    image_prefix: Option<String>,
}

/// Lists active products priced in `currency`. Built dynamically: search,
/// category and sort are all optional.
pub async fn list(
    db: &PgPool,
    storage: &Storage,
    currency: Currency,
    q: ListQuery,
) -> Result<Page<ListItem>, sqlx::Error> {
    let (page, per_page) = paging(q.page, q.per_page);
    let term = q.q.as_deref().map(str::trim).filter(|t| !t.is_empty());

    let mut sql = QueryBuilder::<Postgres>::new(
        "SELECT p.id, p.slug, p.name, pr.price_amount, pr.compare_at_amount,
                EXISTS (SELECT 1 FROM skus s WHERE s.product_id = p.id AND s.active AND s.stock_available > 0) AS in_stock,
                (SELECT i.key_prefix FROM product_images i WHERE i.product_id = p.id
                 ORDER BY i.position, i.created_at LIMIT 1) AS image_prefix
         FROM products p
         JOIN LATERAL (
             SELECT sp.price_amount, sp.compare_at_amount
             FROM skus s JOIN sku_prices sp ON sp.sku_id = s.id
             WHERE s.product_id = p.id AND s.active AND sp.price_currency = ",
    );
    sql.push_bind(currency.code());
    sql.push(" ORDER BY sp.price_amount LIMIT 1) pr ON true WHERE p.status = 'active'");

    if let Some(term) = term {
        // Full-text match, substring match, or close-enough spelling. `<%` is
        // trigram *word* similarity: it compares the query with the best-matching
        // part of the name, so "tudng" still finds "Tudung Bawal Satin". Its
        // threshold is lowered for this query below.
        sql.push(" AND (p.search @@ websearch_to_tsquery('simple', ")
            .push_bind(term)
            .push(") OR p.name ILIKE ")
            .push_bind(like_pattern(term))
            .push(" OR ")
            .push_bind(term)
            .push(" <% p.name)");
    }
    if let Some(category) = &q.category {
        sql.push(
            " AND EXISTS (SELECT 1 FROM product_categories pc JOIN categories c ON c.id = pc.category_id
                          WHERE pc.product_id = p.id AND c.slug = ",
        )
        .push_bind(category)
        .push(")");
    }

    sql.push(" ORDER BY ");
    match (q.sort, term) {
        (Sort::Relevance, Some(term)) => {
            sql.push("GREATEST(ts_rank(p.search, websearch_to_tsquery('simple', ")
                .push_bind(term)
                .push(")), word_similarity(")
                .push_bind(term)
                .push(", p.name)) DESC, ");
        }
        (Sort::PriceAsc, _) => {
            sql.push("pr.price_amount ASC, ");
        }
        (Sort::PriceDesc, _) => {
            sql.push("pr.price_amount DESC, ");
        }
        (Sort::Name, _) => {
            sql.push("p.name ASC, ");
        }
        (Sort::Relevance | Sort::Newest, _) => {}
    }
    // Always end on a unique column so pages are stable.
    sql.push("p.created_at DESC, p.id LIMIT ")
        .push_bind(i64::from(per_page) + 1)
        .push(" OFFSET ")
        .push_bind(i64::from((page - 1) * per_page));

    // Postgres' default word-similarity cut-off (0.6) rejects one-letter typos
    // in short words ("tudng" vs "tudung" scores 0.5). SET LOCAL lowers it for
    // this transaction only, and keeps `<%` able to use the trigram index.
    let mut tx = db.begin().await?;
    sqlx::query("SET LOCAL pg_trgm.word_similarity_threshold = 0.4")
        .execute(&mut *tx)
        .await?;
    let rows = sql.build_query_as::<ListRow>().fetch_all(&mut *tx).await?;
    tx.commit().await?;
    let items = rows
        .into_iter()
        .map(|r| ListItem {
            id: r.id,
            slug: r.slug,
            name: r.name,
            price_from: Money::new(r.price_amount, currency),
            compare_at: r.compare_at_amount.map(|a| Money::new(a, currency)),
            in_stock: r.in_stock,
            thumbnail: r.image_prefix.map(|p| images::urls(storage, &p).thumb),
        })
        .collect();
    Ok(Page::new(items, page, per_page))
}

#[derive(Debug, Serialize)]
pub struct StoreProduct {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub attributes: Value,
    pub currency: Currency,
    pub categories: Vec<CategoryRef>,
    pub images: Vec<ImageView>,
    pub variants: Vec<StoreVariant>,
}

#[derive(Debug, Serialize)]
pub struct CategoryRef {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct StoreVariant {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub options: Value,
    /// `None` when this variant isn't sold in the requested currency.
    pub price: Option<Money>,
    pub compare_at: Option<Money>,
    /// Exact stock levels stay private; shoppers only learn whether they can buy.
    pub in_stock: bool,
}

/// A product page. Inactive products and products with no price in
/// `currency` are reported as not found.
pub async fn get(db: &PgPool, storage: &Storage, slug: &str, currency: Currency) -> Result<StoreProduct, CatalogError> {
    let p = sqlx::query!(
        "SELECT id, slug, name, description, attributes FROM products WHERE slug = $1 AND status = 'active'",
        slug
    )
    .fetch_optional(db)
    .await?
    .ok_or(CatalogError::NotFound)?;

    let variants: Vec<StoreVariant> = sqlx::query!(
        "SELECT s.id, s.code, s.name, s.options, s.stock_available, sp.price_amount AS \"price_amount?\",
                sp.compare_at_amount
         FROM skus s LEFT JOIN sku_prices sp ON sp.sku_id = s.id AND sp.price_currency = $2
         WHERE s.product_id = $1 AND s.active
         ORDER BY s.position, s.created_at",
        p.id,
        currency.code()
    )
    .fetch_all(db)
    .await?
    .into_iter()
    .map(|v| StoreVariant {
        id: v.id,
        code: v.code,
        name: v.name,
        options: v.options,
        price: v.price_amount.map(|a| Money::new(a, currency)),
        compare_at: v.compare_at_amount.map(|a| Money::new(a, currency)),
        in_stock: v.stock_available > 0,
    })
    .collect();
    if !variants.iter().any(|v| v.price.is_some()) {
        return Err(CatalogError::NotFound);
    }

    let categories = sqlx::query_as!(
        CategoryRef,
        "SELECT c.id, c.slug, c.name FROM categories c
         JOIN product_categories pc ON pc.category_id = c.id
         WHERE pc.product_id = $1 ORDER BY c.position, c.name",
        p.id
    )
    .fetch_all(db)
    .await?;

    Ok(StoreProduct {
        images: images::for_product(db, storage, p.id).await?,
        id: p.id,
        slug: p.slug,
        name: p.name,
        description: p.description,
        attributes: p.attributes,
        currency,
        categories,
        variants,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_patterns_escape_wildcards() {
        assert_eq!(like_pattern("50%_off"), "%50\\%\\_off%");
    }

    #[test]
    fn paging_is_clamped() {
        assert_eq!(paging(None, None), (1, 24));
        assert_eq!(paging(Some(0), Some(1000)), (1, 100));
    }

    #[test]
    fn page_uses_extra_row_for_has_more() {
        let p = Page::new(vec![1, 2, 3], 1, 2);
        assert_eq!(p.items, [1, 2]);
        assert!(p.has_more);
        assert!(!Page::new(vec![1, 2], 1, 2).has_more);
    }
}
