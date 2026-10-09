//! Categories. They nest (`parent_id`), and the API returns them as a flat list
//! ordered by `position`; storefronts build the tree they want from it.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::{CatalogError, double_option, map_unique, require_name, slugify, validate_slug};

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Category {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub position: i32,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct NewCategory {
    pub name: String,
    /// Made from the name when left out.
    pub slug: Option<String>,
    pub parent_id: Option<Uuid>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub position: i32,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CategoryPatch {
    pub name: Option<String>,
    pub slug: Option<String>,
    /// `null` moves the category to the top level.
    #[serde(default, deserialize_with = "double_option")]
    pub parent_id: Option<Option<Uuid>>,
    pub description: Option<String>,
    pub position: Option<i32>,
}

pub async fn list(db: &PgPool) -> Result<Vec<Category>, sqlx::Error> {
    sqlx::query_as!(
        Category,
        "SELECT id, parent_id, slug, name, description, position FROM categories ORDER BY position, name"
    )
    .fetch_all(db)
    .await
}

pub async fn create(db: &PgPool, new: NewCategory) -> Result<Category, CatalogError> {
    let name = require_name(&new.name)?;
    let slug = new.slug.unwrap_or_else(|| slugify(&name));
    validate_slug(&slug)?;
    if let Some(parent) = new.parent_id {
        ensure_exists(db, parent).await?;
    }
    sqlx::query_as!(
        Category,
        "INSERT INTO categories (id, parent_id, slug, name, description, position)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id, parent_id, slug, name, description, position",
        Uuid::now_v7(),
        new.parent_id,
        slug,
        name,
        new.description,
        new.position,
    )
    .fetch_one(db)
    .await
    .map_err(map_unique)
}

pub async fn update(db: &PgPool, id: Uuid, patch: CategoryPatch) -> Result<Category, CatalogError> {
    let name = patch.name.as_deref().map(require_name).transpose()?;
    if let Some(slug) = &patch.slug {
        validate_slug(slug)?;
    }
    if let Some(Some(parent)) = patch.parent_id {
        ensure_exists(db, parent).await?;
        // Walk up from the new parent; finding ourselves means a cycle.
        let cycle = sqlx::query_scalar!(
            r#"WITH RECURSIVE ancestors AS (
                   SELECT id, parent_id FROM categories WHERE id = $1
                   UNION ALL
                   SELECT c.id, c.parent_id FROM categories c JOIN ancestors a ON c.id = a.parent_id
               )
               SELECT EXISTS (SELECT 1 FROM ancestors WHERE id = $2) AS "cycle!""#,
            parent,
            id
        )
        .fetch_one(db)
        .await?;
        if cycle {
            return Err(CatalogError::InvalidInput(
                "a category can't be placed inside itself or its own subcategory".into(),
            ));
        }
    }
    sqlx::query_as!(
        Category,
        "UPDATE categories SET
             name        = COALESCE($2, name),
             slug        = COALESCE($3, slug),
             parent_id   = CASE WHEN $4 THEN $5 ELSE parent_id END,
             description = COALESCE($6, description),
             position    = COALESCE($7, position),
             updated_at  = now()
         WHERE id = $1
         RETURNING id, parent_id, slug, name, description, position",
        id,
        name,
        patch.slug,
        patch.parent_id.is_some(),
        patch.parent_id.flatten(),
        patch.description,
        patch.position,
    )
    .fetch_optional(db)
    .await
    .map_err(map_unique)?
    .ok_or(CatalogError::NotFound)
}

/// Deletes a category. Its products stay; they just lose this category.
pub async fn delete(db: &PgPool, id: Uuid) -> Result<(), CatalogError> {
    let deleted = sqlx::query!("DELETE FROM categories WHERE id = $1", id)
        .execute(db)
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(CatalogError::NotFound);
    }
    Ok(())
}

async fn ensure_exists(db: &PgPool, id: Uuid) -> Result<(), CatalogError> {
    let exists = sqlx::query_scalar!(r#"SELECT EXISTS (SELECT 1 FROM categories WHERE id = $1) AS "e!""#, id)
        .fetch_one(db)
        .await?;
    if exists {
        Ok(())
    } else {
        Err(CatalogError::InvalidInput(format!("category {id} doesn't exist")))
    }
}
