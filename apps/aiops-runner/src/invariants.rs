//! Response invariants: rules every successful response must follow, whatever the code that
//! produced it. Golden cases only say "same as the original"; invariants say "correct per the
//! API's own rules", so they also catch bugs the original already had. They run on the baseline
//! and the candidate alike.

use aiops_core::report::{CaseResult, CorrectnessReport};
use serde_json::Value;

/// Most mismatches listed in a report; the counts always cover every response.
const MAX_LISTED: usize = 20;

/// Invariant sets the runner knows. A scenario names the ones that apply to its API.
pub const KNOWN: &[&str] = &["orders-page"];

/// One fetched response: request path and either `(status, body)` or the request error.
pub type Response = (String, Result<(u16, Value), String>);

/// Checks every successful response against the named invariant sets.
pub fn check(sets: &[String], responses: &[Response]) -> CorrectnessReport {
    let mut cases = Vec::new();
    let mut checked = 0;
    let mut failed = 0;
    for (path, result) in responses {
        let Ok((200, body)) = result else { continue };
        checked += 1;
        let mut violations = Vec::new();
        for set in sets {
            if set == "orders-page" {
                violations.extend(orders_page(path, body));
            }
        }
        if !violations.is_empty() {
            failed += 1;
            if cases.len() < MAX_LISTED {
                violations.truncate(3);
                cases.push(CaseResult {
                    name: path.clone(),
                    passed: false,
                    detail: Some(violations.join("; ")),
                });
            }
        }
    }
    CorrectnessReport {
        total: checked,
        passed: checked - failed,
        cases,
    }
}

/// `GET /users/{user_id}/orders?limit=&offset=` (defaults 20 and 0).
fn orders_page(path: &str, body: &Value) -> Vec<String> {
    let Some((user, limit, offset)) = parse_orders_path(path) else {
        return vec!["not an order-list request".into()];
    };
    let mut v = Vec::new();
    let int = |x: &Value| x.as_i64();
    if int(&body["user_id"]) != Some(user) {
        v.push(format!("user_id is {}, expected {user}", body["user_id"]));
    }
    if int(&body["limit"]) != Some(limit) || int(&body["offset"]) != Some(offset) {
        v.push(format!(
            "page is limit {} offset {}, requested {limit} and {offset}",
            body["limit"], body["offset"]
        ));
    }
    let Some(orders) = body["orders"].as_array() else {
        v.push("orders is not a list".into());
        return v;
    };
    if orders.len() as i64 > limit {
        v.push(format!("{} orders for limit {limit}", orders.len()));
    }
    let mut previous: Option<(&str, i64)> = None;
    for (i, order) in orders.iter().enumerate() {
        let at = format!("orders[{i}]");
        if int(&order["user_id"]) != Some(user) {
            v.push(format!("{at} belongs to user {}", order["user_id"]));
        }
        let key = (
            order["created_at"].as_str().unwrap_or(""),
            int(&order["id"]).unwrap_or(0),
        );
        // Newest first, ties broken by the higher id: strictly decreasing, so no duplicates.
        if let Some(prev) = previous {
            if key >= prev {
                v.push(format!(
                    "{at} is out of order (newest first, then highest id)"
                ));
            }
        }
        previous = Some(key);
        let Some(items) = order["items"].as_array() else {
            v.push(format!("{at}.items is not a list"));
            continue;
        };
        if int(&order["item_count"]) != Some(items.len() as i64) {
            v.push(format!(
                "{at}.item_count is {}, but it lists {} items",
                order["item_count"],
                items.len()
            ));
        }
        let mut sum = 0;
        for (j, item) in items.iter().enumerate() {
            let at = format!("{at}.items[{j}]");
            let quantity = int(&item["quantity"]).unwrap_or(0);
            let price = int(&item["unit_price_cents"]).unwrap_or(-1);
            let line = int(&item["line_total_cents"]).unwrap_or(i64::MIN);
            if quantity < 1 || price < 0 {
                v.push(format!(
                    "{at} has quantity {quantity} and unit price {price}"
                ));
            }
            if line != quantity * price {
                v.push(format!(
                    "{at}.line_total_cents is {line}, expected {quantity} × {price}"
                ));
            }
            if item["product_name"].as_str().is_none_or(str::is_empty) {
                v.push(format!("{at}.product_name is missing"));
            }
            sum += line;
        }
        if int(&order["total_cents"]) != Some(sum) {
            v.push(format!(
                "{at}.total_cents is {}, but its lines add up to {sum}",
                order["total_cents"]
            ));
        }
    }
    v
}

fn parse_orders_path(path: &str) -> Option<(i64, i64, i64)> {
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    let user = route
        .strip_prefix("/users/")?
        .strip_suffix("/orders")?
        .parse()
        .ok()?;
    let (mut limit, mut offset) = (20, 0);
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        match pair.split_once('=')? {
            ("limit", n) => limit = n.parse().ok()?,
            ("offset", n) => offset = n.parse().ok()?,
            _ => {}
        }
    }
    Some((user, limit, offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page() -> Value {
        json!({
            "user_id": 7, "limit": 2, "offset": 0,
            "orders": [
                {"id": 9, "user_id": 7, "status": "paid", "created_at": "2026-03-02T00:00:00Z",
                 "item_count": 2, "total_cents": 700,
                 "items": [
                    {"product_id": 1, "product_name": "a", "quantity": 2, "unit_price_cents": 200, "line_total_cents": 400},
                    {"product_id": 2, "product_name": "b", "quantity": 1, "unit_price_cents": 300, "line_total_cents": 300}]},
                {"id": 4, "user_id": 7, "status": "paid", "created_at": "2026-03-01T00:00:00Z",
                 "item_count": 0, "total_cents": 0, "items": []}
            ]
        })
    }

    fn violations(body: Value) -> Vec<String> {
        orders_page("/users/7/orders?limit=2", &body)
    }

    #[test]
    fn a_correct_page_passes() {
        assert_eq!(violations(page()), Vec::<String>::new());
        assert_eq!(
            parse_orders_path("/users/3/orders?limit=10&offset=25"),
            Some((3, 10, 25))
        );
        assert_eq!(parse_orders_path("/users/3/orders"), Some((3, 20, 0)));
    }

    #[test]
    fn each_rule_is_enforced() {
        let mut p = page();
        p["orders"][0]["items"][0]["line_total_cents"] = json!(399);
        let v = violations(p);
        assert!(
            v.iter().any(|m| m.contains("line_total_cents is 399")),
            "{v:?}"
        );
        assert!(v.iter().any(|m| m.contains("lines add up to 699")), "{v:?}");

        let mut p = page();
        p["orders"][1]["user_id"] = json!(8);
        assert!(violations(p)
            .iter()
            .any(|m| m.contains("belongs to user 8")));

        let mut p = page();
        p["orders"][1]["created_at"] = json!("2026-03-03T00:00:00Z");
        assert!(violations(p).iter().any(|m| m.contains("out of order")));

        let mut p = page();
        p["orders"][0]["item_count"] = json!(3);
        assert!(violations(p).iter().any(|m| m.contains("item_count is 3")));

        let mut p = page();
        p["orders"][0]["items"][1]["product_name"] = json!("");
        assert!(violations(p)
            .iter()
            .any(|m| m.contains("product_name is missing")));

        let mut p = page();
        p["offset"] = json!(1);
        assert!(violations(p)
            .iter()
            .any(|m| m.contains("requested 2 and 0")));
    }

    #[test]
    fn only_successful_responses_are_checked() {
        let sets = vec!["orders-page".to_string()];
        let responses: Vec<Response> = vec![
            ("/users/7/orders?limit=2".into(), Ok((200, page()))),
            (
                "/users/9/orders".into(),
                Ok((404, json!({"error": "user_not_found"}))),
            ),
            ("/users/9/orders".into(), Err("connection refused".into())),
        ];
        let r = check(&sets, &responses);
        assert_eq!((r.total, r.passed), (1, 1));
    }
}
