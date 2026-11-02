-- Deterministic seed: the same rows on every run, no random().
-- 2,010 users (users 2001–2010 have no orders), 1,000 products, 60,000 orders (30 per user),
-- 1–5 line items per order (180,000 in total).

INSERT INTO users (id, name)
SELECT g, 'user-' || g
FROM generate_series(1, 2010) AS g;

INSERT INTO products (id, name, price_cents)
SELECT g, 'product-' || lpad(g::text, 4, '0'), 100 + (g * 37) % 9900
FROM generate_series(1, 1000) AS g;

-- 7919 is coprime with 2000, so every user in 1..2000 gets exactly 30 orders.
INSERT INTO orders (id, user_id, status, created_at)
SELECT g,
       1 + (g * 7919) % 2000,
       (ARRAY['paid', 'shipped', 'delivered', 'cancelled'])[1 + (g * 13) % 4],
       TIMESTAMPTZ '2026-01-01 00:00:00+00' + make_interval(secs => (g * 104729) % 25000000)
FROM generate_series(1::bigint, 60000::bigint) AS g;

INSERT INTO order_items (id, order_id, product_id, quantity, unit_price_cents)
SELECT row_number() OVER (ORDER BY o.id, k),
       o.id,
       p.id,
       1 + (o.id + k) % 4,
       p.price_cents
FROM orders AS o
CROSS JOIN LATERAL generate_series(1, 1 + (o.id % 5)::int) AS k
JOIN products AS p ON p.id = 1 + ((o.id * 31 + k * 17) % 1000);

ANALYZE;
