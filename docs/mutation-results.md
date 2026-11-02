# 변이 테스트 결과 (로컬, scripts/mutation-test.py)

36 mutants: 36 compiled and ran, 0 did not (none)
golden answers alone: 30/36 killed (83%)
golden + invariants + sample: 30/36 killed (83%)

| id | operator | location | change | result | caught by |
| --- | --- | --- | --- | --- | --- |
| m01 | sql: = -> >= | src/repo.rs:8 | `.run(sqlx::query("SELECT 1 FROM users WHERE id >= $1").bind(user_id).fetch_optio` | killed | golden |
| m02 | sql: = -> <> | src/repo.rs:8 | `.run(sqlx::query("SELECT 1 FROM users WHERE id <> $1").bind(user_id).fetch_optio` | killed | golden |
| m03 | bind: +1 | src/repo.rs:8 | `.run(sqlx::query("SELECT 1 FROM users WHERE id = $1").bind(user_id + 1).fetch_op` | killed | golden |
| m04 | bind: -1 | src/repo.rs:8 | `.run(sqlx::query("SELECT 1 FROM users WHERE id = $1").bind(user_id - 1).fetch_op` | killed | golden, sample |
| m05 | negate: is_some -> is_none | src/repo.rs:10 | `Ok(row.is_none())` | killed | golden, sample |
| m06 | sql: = -> >= | src/repo.rs:25 | `FROM orders WHERE user_id >= $1 \` | killed | golden, invariants, sample |
| m07 | sql: = -> <> | src/repo.rs:25 | `FROM orders WHERE user_id <> $1 \` | killed | golden, invariants, sample |
| m08 | sql: DESC -> ASC | src/repo.rs:26 | `ORDER BY created_at ASC, id DESC LIMIT $2 OFFSET $3",` | killed | golden, invariants, sample |
| m09 | sql: DESC -> ASC | src/repo.rs:26 | `ORDER BY created_at DESC, id ASC LIMIT $2 OFFSET $3",` | survived | - |
| m10 | sql: drop the id tie-break | src/repo.rs:26 | `ORDER BY created_at DESC LIMIT $2 OFFSET $3",` | survived | - |
| m11 | sql: swap LIMIT and OFFSET | src/repo.rs:26 | `ORDER BY created_at DESC, id DESC LIMIT $3 OFFSET $2",` | killed | golden, invariants, sample |
| m12 | bind: +1 | src/repo.rs:28 | `.bind(user_id + 1)` | killed | golden, invariants, sample |
| m13 | bind: -1 | src/repo.rs:28 | `.bind(user_id - 1)` | killed | golden, invariants, sample |
| m14 | bind: +1 | src/repo.rs:29 | `.bind(limit + 1)` | killed | golden, invariants, sample |
| m15 | bind: -1 | src/repo.rs:29 | `.bind(limit - 1)` | killed | golden, sample |
| m16 | bind: +1 | src/repo.rs:30 | `.bind(offset + 1)` | killed | golden, sample |
| m17 | bind: -1 | src/repo.rs:30 | `.bind(offset - 1)` | killed | golden, sample |
| m18 | alloc: with_capacity -> new | src/repo.rs:51 | `let mut views = Vec::new();` | survived | - |
| m19 | loop: skip the first | src/repo.rs:52 | `for order in orders.into_iter().skip(1) {` | killed | golden, sample |
| m20 | loop: reverse | src/repo.rs:52 | `for order in orders.into_iter().rev() {` | killed | golden, invariants, sample |
| m21 | sql: = -> >= | src/repo.rs:57 | `WHERE order_id >= $1 ORDER BY id",` | killed | golden, sample |
| m22 | sql: = -> <> | src/repo.rs:57 | `WHERE order_id <> $1 ORDER BY id",` | killed | golden, sample |
| m23 | sql: drop item order | src/repo.rs:57 | `WHERE order_id = $1",` | survived | - |
| m24 | bind: +1 | src/repo.rs:59 | `.bind(order.id + 1)` | killed | golden, sample |
| m25 | bind: -1 | src/repo.rs:59 | `.bind(order.id - 1)` | killed | golden, sample |
| m26 | alloc: with_capacity -> new | src/repo.rs:63 | `let mut items = Vec::new();` | survived | - |
| m27 | loop: skip the first | src/repo.rs:64 | `for row in item_rows.into_iter().skip(1) {` | killed | golden, sample |
| m28 | loop: reverse | src/repo.rs:64 | `for row in item_rows.into_iter().rev() {` | killed | golden, sample |
| m29 | sql: = -> >= | src/repo.rs:67 | `.run(sqlx::query("SELECT name FROM products WHERE id >= $1").bind(product_id).fe` | survived | - |
| m30 | sql: = -> <> | src/repo.rs:67 | `.run(sqlx::query("SELECT name FROM products WHERE id <> $1").bind(product_id).fe` | killed | golden, sample |
| m31 | bind: +1 | src/repo.rs:67 | `.run(sqlx::query("SELECT name FROM products WHERE id = $1").bind(product_id + 1)` | killed | golden, sample |
| m32 | bind: -1 | src/repo.rs:67 | `.run(sqlx::query("SELECT name FROM products WHERE id = $1").bind(product_id - 1)` | killed | golden, sample |
| m33 | delete: push of an order | src/repo.rs:76 | `// (removed)` | killed | golden, sample |
| m34 | arith: * -> + | src/model.rs:46 | `line_total_cents: quantity as i64 + unit_price_cents as i64,` | killed | golden, invariants, sample |
| m35 | count: len + 1 | src/model.rs:58 | `item_count: items.len() as i64 + 1,` | killed | golden, invariants, sample |
| m36 | sum: first line only | src/model.rs:59 | `total_cents: items.iter().map(\|i\| i.line_total_cents).take(1).sum(),` | killed | golden, invariants, sample |
