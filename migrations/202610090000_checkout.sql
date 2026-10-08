-- Checkout: shipping zones and rates, tax rules, orders and idempotency keys.

-- A shipping zone is a set of regions (a whole country, or states within one)
-- that share the same rates, e.g. "West Malaysia", "East Malaysia", "Singapore".
CREATE TABLE shipping_zones (
    id         UUID PRIMARY KEY,
    name       TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE shipping_zone_regions (
    zone_id UUID NOT NULL REFERENCES shipping_zones (id) ON DELETE CASCADE,
    -- ISO 3166-1 alpha-2, uppercase.
    country CHAR(2) NOT NULL,
    -- Uppercase state/province code; empty string means the whole country.
    state   TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (zone_id, country, state)
);

CREATE INDEX shipping_zone_regions_lookup_idx ON shipping_zone_regions (country, state);

-- Rates within a zone. A rate applies when the parcel weight falls in its
-- range and it is priced in the order currency.
CREATE TABLE shipping_rates (
    id                UUID PRIMARY KEY,
    zone_id           UUID NOT NULL REFERENCES shipping_zones (id) ON DELETE CASCADE,
    name              TEXT NOT NULL,
    price_currency    CHAR(3) NOT NULL,
    price_amount      BIGINT NOT NULL CHECK (price_amount >= 0),
    min_weight_g      INTEGER NOT NULL DEFAULT 0 CHECK (min_weight_g >= 0),
    -- NULL means no upper limit.
    max_weight_g      INTEGER CHECK (max_weight_g IS NULL OR max_weight_g >= min_weight_g),
    -- Free shipping when the order subtotal reaches this (same currency). NULL = never.
    free_over_amount  BIGINT CHECK (free_over_amount IS NULL OR free_over_amount >= 0),
    active            BOOLEAN NOT NULL DEFAULT true,
    position          INTEGER NOT NULL DEFAULT 0,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX shipping_rates_zone_idx ON shipping_rates (zone_id, position);

-- Tax by destination. The most specific matching rule wins: a postcode
-- prefix beats a state, which beats the whole country. No match falls back
-- to the configured default rate.
CREATE TABLE tax_rules (
    id                  UUID PRIMARY KEY,
    name                TEXT NOT NULL,
    country             CHAR(2) NOT NULL,
    state               TEXT NOT NULL DEFAULT '',
    postcode_prefix     TEXT NOT NULL DEFAULT '',
    -- Hundredths of a percent: 6% = 600, 8.25% = 825.
    rate_bp             INTEGER NOT NULL CHECK (rate_bp >= 0 AND rate_bp <= 10000),
    applies_to_shipping BOOLEAN NOT NULL DEFAULT false,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (country, state, postcode_prefix)
);

-- Human-friendly order numbers, separate from the UUID primary key.
CREATE SEQUENCE order_number_seq START 1001;

CREATE TABLE orders (
    id                 UUID PRIMARY KEY,
    number             BIGINT NOT NULL UNIQUE DEFAULT nextval('order_number_seq'),
    status             TEXT NOT NULL CHECK (status IN ('pending_payment', 'paid', 'fulfilled', 'cancelled', 'expired')),
    email              TEXT NOT NULL,
    customer_id        UUID REFERENCES users (id) ON DELETE SET NULL,
    -- One currency for the whole order: every amount below is in it.
    currency           CHAR(3) NOT NULL,
    subtotal_amount    BIGINT NOT NULL,
    shipping_amount    BIGINT NOT NULL,
    tax_amount         BIGINT NOT NULL,
    total_amount       BIGINT NOT NULL,
    prices_include_tax BOOLEAN NOT NULL,
    -- Snapshots, so later edits to rates or rules never change a placed order.
    tax_name           TEXT NOT NULL,
    tax_rate_bp        INTEGER NOT NULL,
    shipping_option    JSONB NOT NULL,
    shipping_address   JSONB NOT NULL,
    notes              TEXT NOT NULL DEFAULT '',
    -- SHA-256 of the token that lets a guest view their order.
    access_token_hash  BYTEA NOT NULL,
    -- Stock is held for unpaid orders until this moment.
    expires_at         TIMESTAMPTZ NOT NULL,
    paid_at            TIMESTAMPTZ,
    cancelled_at       TIMESTAMPTZ,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX orders_status_created_idx ON orders (status, created_at DESC);
CREATE INDEX orders_pending_expiry_idx ON orders (expires_at) WHERE status = 'pending_payment';
CREATE INDEX orders_customer_idx ON orders (customer_id, created_at DESC);
CREATE INDEX orders_email_idx ON orders (email);

CREATE TABLE order_lines (
    id              UUID PRIMARY KEY,
    order_id        UUID NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    -- RESTRICT: a SKU that has been ordered can't be deleted out from under its orders.
    sku_id          UUID NOT NULL REFERENCES skus (id),
    product_id      UUID NOT NULL REFERENCES products (id),
    product_name    TEXT NOT NULL,
    sku_code        TEXT NOT NULL,
    sku_name        TEXT NOT NULL,
    quantity        INTEGER NOT NULL CHECK (quantity > 0),
    unit_amount     BIGINT NOT NULL,
    subtotal_amount BIGINT NOT NULL,
    tax_amount      BIGINT NOT NULL,
    position        INTEGER NOT NULL
);

CREATE INDEX order_lines_order_idx ON order_lines (order_id, position);
CREATE INDEX order_lines_sku_idx ON order_lines (sku_id);

-- Remembers the response to a request sent with an Idempotency-Key, so a
-- retried checkout returns the original order instead of placing another.
CREATE TABLE idempotency_keys (
    scope         TEXT NOT NULL,
    key           TEXT NOT NULL,
    -- Same key with a different request body is a client bug, rejected.
    request_hash  BYTEA NOT NULL,
    response_body JSONB NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (scope, key)
);

CREATE INDEX idempotency_keys_created_idx ON idempotency_keys (created_at);
