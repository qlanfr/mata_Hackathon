-- Vulnerability-fix scenario: the order API's schema, with an email per user (data an attacker
-- wants and no product search should ever return).
CREATE TABLE users (
    id    BIGINT PRIMARY KEY,
    name  TEXT   NOT NULL,
    email TEXT   NOT NULL
);

CREATE TABLE products (
    id          BIGINT PRIMARY KEY,
    name        TEXT   NOT NULL,
    price_cents INT    NOT NULL
);

CREATE TABLE orders (
    id         BIGINT      PRIMARY KEY,
    user_id    BIGINT      NOT NULL REFERENCES users (id),
    status     TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX orders_user_created_idx ON orders (user_id, created_at DESC, id DESC);

CREATE TABLE order_items (
    id               BIGINT PRIMARY KEY,
    order_id         BIGINT NOT NULL REFERENCES orders (id),
    product_id       BIGINT NOT NULL REFERENCES products (id),
    quantity         INT    NOT NULL,
    unit_price_cents INT    NOT NULL
);
CREATE INDEX order_items_order_idx ON order_items (order_id, id);
