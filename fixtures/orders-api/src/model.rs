use serde::Serialize;

#[derive(Serialize)]
pub struct OrderPage {
    pub user_id: i64,
    pub limit: i64,
    pub offset: i64,
    pub orders: Vec<OrderView>,
}

#[derive(Serialize)]
pub struct OrderView {
    pub id: i64,
    pub user_id: i64,
    pub status: String,
    pub created_at: String,
    pub item_count: i64,
    pub total_cents: i64,
    pub items: Vec<ItemView>,
}

#[derive(Serialize)]
pub struct ItemView {
    pub product_id: i64,
    pub product_name: String,
    pub quantity: i32,
    pub unit_price_cents: i32,
    pub line_total_cents: i64,
}

/// An order row before its line items are loaded.
pub struct OrderRow {
    pub id: i64,
    pub user_id: i64,
    pub status: String,
    pub created_at: String,
}

impl ItemView {
    pub fn new(product_id: i64, product_name: String, quantity: i32, unit_price_cents: i32) -> Self {
        Self {
            product_id,
            product_name,
            quantity,
            unit_price_cents,
            line_total_cents: quantity as i64 * unit_price_cents as i64,
        }
    }
}

impl OrderView {
    pub fn from_parts(row: OrderRow, items: Vec<ItemView>) -> Self {
        Self {
            id: row.id,
            user_id: row.user_id,
            status: row.status,
            created_at: row.created_at,
            item_count: items.len() as i64,
            total_cents: items.iter().map(|i| i.line_total_cents).sum(),
            items,
        }
    }
}
