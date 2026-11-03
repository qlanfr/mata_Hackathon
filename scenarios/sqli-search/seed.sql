-- Deterministic seed: 100 users with emails, 1,000 products with searchable names, 300 orders.
INSERT INTO users (id, name, email)
SELECT g, 'user-' || g, 'user' || g || '@example.test'
FROM generate_series(1, 100) AS g;

INSERT INTO products (id, name, price_cents)
SELECT g,
       (ARRAY['chair', 'desk', 'lamp', 'mug', 'cable', 'monitor', 'keyboard', 'mouse'])[1 + g % 8]
           || ' ' || lpad(g::text, 4, '0'),
       100 + (g * 37) % 9900
FROM generate_series(1, 1000) AS g;

INSERT INTO orders (id, user_id, status, created_at)
SELECT g,
       1 + g % 100,
       (ARRAY['paid', 'shipped', 'delivered', 'cancelled'])[1 + (g * 13) % 4],
       TIMESTAMPTZ '2026-01-01 00:00:00+00' + make_interval(secs => (g * 104729) % 25000000)
FROM generate_series(1::bigint, 300::bigint) AS g;

INSERT INTO order_items (id, order_id, product_id, quantity, unit_price_cents)
SELECT row_number() OVER (ORDER BY o.id, k),
       o.id,
       p.id,
       1 + (o.id + k) % 4,
       p.price_cents
FROM orders AS o
CROSS JOIN LATERAL generate_series(1, 1 + (o.id % 3)::int) AS k
JOIN products AS p ON p.id = 1 + ((o.id * 31 + k * 17) % 1000);

ANALYZE;
