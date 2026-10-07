-- Baseline: extensions and the shop-wide settings store.

-- Trigram indexes power fuzzy product search without an external search engine.
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- Small key/value store for runtime settings an admin can change without a
-- redeploy (e.g. whether first-time setup has completed).
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
