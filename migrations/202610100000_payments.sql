-- Payments taken through a payment adapter (e.g. HitPay), plus a review flag
-- on orders for payments that need a human decision.

CREATE TABLE payments (
    id               UUID PRIMARY KEY,
    order_id         UUID NOT NULL REFERENCES orders (id),
    -- Which adapter took it, and its id on the provider's side.
    adapter          TEXT NOT NULL,
    provider_ref     TEXT NOT NULL,
    status           TEXT NOT NULL CHECK (status IN ('pending', 'succeeded', 'failed')),
    -- What we asked for; must match the order's currency.
    requested_amount BIGINT NOT NULL,
    currency         CHAR(3) NOT NULL,
    -- Where to send the shopper to pay, and/or a QR payload to render.
    checkout_url     TEXT,
    qr_code          TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at     TIMESTAMPTZ,
    UNIQUE (adapter, provider_ref)
);

CREATE INDEX payments_order_idx ON payments (order_id, created_at DESC);
CREATE INDEX payments_pending_idx ON payments (created_at) WHERE status = 'pending';

-- Every webhook we accept, kept for auditing and debugging.
CREATE TABLE payment_events (
    id          UUID PRIMARY KEY,
    adapter     TEXT NOT NULL,
    payment_id  UUID REFERENCES payments (id),
    payload     JSONB NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX payment_events_payment_idx ON payment_events (payment_id);

ALTER TABLE orders
    -- Set when money arrived but the order couldn't simply be marked paid
    -- (e.g. paid after expiry and the stock had sold out). Staff resolve it.
    ADD COLUMN review_reason TEXT,
    -- How the order was paid: an adapter id, or 'manual' when staff marked it.
    ADD COLUMN paid_via TEXT;

CREATE INDEX orders_needs_review_idx ON orders (created_at DESC) WHERE review_reason IS NOT NULL;
