-- Product catalog: categories, products, their sellable variants (SKUs),
-- per-currency prices and images.

CREATE TABLE categories (
    id          UUID PRIMARY KEY,
    -- Categories nest; deleting a parent lifts its children to the top level.
    parent_id   UUID REFERENCES categories (id) ON DELETE SET NULL,
    slug        TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    position    INTEGER NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE products (
    id          UUID PRIMARY KEY,
    slug        TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    -- Only 'active' products are visible to shoppers. Products are archived,
    -- never deleted, so past orders can always point at them.
    status      TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'archived')),
    -- Free-form details for the storefront (material, care instructions…).
    attributes  JSONB NOT NULL DEFAULT '{}',
    -- 'simple' text search config: no language-specific stemming, so mixed
    -- English/Malay/Chinese names all search the same way.
    search      TSVECTOR GENERATED ALWAYS AS (
                    setweight(to_tsvector('simple', name), 'A') ||
                    setweight(to_tsvector('simple', description), 'B')
                ) STORED,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX products_search_idx ON products USING GIN (search);
-- Trigram index: typo-tolerant and substring matching on names.
CREATE INDEX products_name_trgm_idx ON products USING GIN (name gin_trgm_ops);
CREATE INDEX products_status_idx ON products (status, created_at DESC);

CREATE TABLE product_categories (
    product_id  UUID NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    category_id UUID NOT NULL REFERENCES categories (id) ON DELETE CASCADE,
    PRIMARY KEY (product_id, category_id)
);

CREATE INDEX product_categories_category_idx ON product_categories (category_id);

-- A SKU is one buyable variant of a product ("Red / L"). Every product has at
-- least one before it can be sold.
CREATE TABLE skus (
    id              UUID PRIMARY KEY,
    product_id      UUID NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    code            TEXT NOT NULL UNIQUE,
    name            TEXT NOT NULL DEFAULT '',
    -- Variant choices, e.g. {"colour": "red", "size": "L"}.
    options         JSONB NOT NULL DEFAULT '{}',
    -- Units that can still be sold: on-hand stock minus units held by unpaid
    -- orders. The CHECK is the last line of defence against overselling.
    stock_available INTEGER NOT NULL DEFAULT 0 CHECK (stock_available >= 0),
    weight_g        INTEGER NOT NULL DEFAULT 0 CHECK (weight_g >= 0),
    active          BOOLEAN NOT NULL DEFAULT true,
    position        INTEGER NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX skus_product_idx ON skus (product_id, position);

-- One price per SKU per currency. No automatic FX: a SKU without a price in a
-- currency simply isn't sold in that currency.
CREATE TABLE sku_prices (
    sku_id            UUID NOT NULL REFERENCES skus (id) ON DELETE CASCADE,
    price_currency    CHAR(3) NOT NULL,
    price_amount      BIGINT NOT NULL CHECK (price_amount >= 0),
    -- Optional "was" price shown struck through; same currency as the price.
    compare_at_amount BIGINT CHECK (compare_at_amount >= 0),
    PRIMARY KEY (sku_id, price_currency)
);

CREATE TABLE product_images (
    id         UUID PRIMARY KEY,
    product_id UUID NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    -- Set when the image shows one specific variant.
    sku_id     UUID REFERENCES skus (id) ON DELETE SET NULL,
    -- Storage key prefix; resized files live under it (…/large.webp, …/thumb.webp).
    -- Content-addressed, so the same upload twice is stored once.
    key_prefix TEXT NOT NULL,
    width      INTEGER NOT NULL,
    height     INTEGER NOT NULL,
    alt        TEXT NOT NULL DEFAULT '',
    position   INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX product_images_product_idx ON product_images (product_id, position);
CREATE INDEX product_images_key_prefix_idx ON product_images (key_prefix);
