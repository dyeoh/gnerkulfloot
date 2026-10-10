-- When staff marked an order as sent.
ALTER TABLE orders ADD COLUMN fulfilled_at TIMESTAMPTZ;
