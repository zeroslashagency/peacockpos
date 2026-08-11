//! Lane 4A-1 acceptance tests: the order endpoints against a real PostgreSQL.
//!
//! Each test gets its own freshly migrated database (`support::TestApp`), so a green run is
//! also evidence that 009_order_lifecycle.sql applies cleanly on top of 001-008.
//!
//! ## What these tests are for
//!
//! The Lane 3D tests in `src/routes/orders.rs` already prove the *handlers* behave, driving
//! them over the in-memory store. They cannot prove the thing this lane delivers: that a
//! `201` corresponds to a committed row, that a replay does not insert a second one, and
//! that the numbering stays gapless when the counter is a Postgres row rather than a
//! `HashMap` entry. So every assertion here checks the response **and** the database.
//!
//! ## Why the row-level assertions matter
//!
//! An endpoint that answers `201 Created` and writes nowhere is exactly the failure this
//! lane exists to close, and it is invisible to a test that only reads the response body.
//! `TestApp::order_count`, `invoice_names` and `order_row` query the tables directly.

mod support;

use serde_json::json;
use support::{invoice_body, order_body, TestApp, BUSINESS_DATE, SERIES};
use uuid::Uuid;

use axum::http::StatusCode;

/// Create an order through the API and return its wire id.
async fn create_order(app: &TestApp, table: &str) -> String {
    let (status, body) = app.post("/api/orders", &order_body(table)).await;
    assert_eq!(status, StatusCode::CREATED, "create failed: {body}");
    body["id"].as_str().expect("id in response").to_owned()
}

// ---------------------------------------------------------------------------
// 1. The wiring itself
// ---------------------------------------------------------------------------

#[tokio::test]
async fn migrations_apply_and_every_lane_four_table_exists() {
    let app = TestApp::new().await;

    // 009 is this lane's; the rest must still be there, since a migration that dropped a
    // predecessor's table would pass every other test in this file.
    for table in [
        "orders",
        "order_items",
        "order_idempotency_keys",
        "invoices",
        "invoice_lines",
        "invoice_naming_series",
        "idempotency_keys",
        "tables",
        "items",
    ] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM information_schema.tables
                 WHERE table_schema = 'public' AND table_name = $1
             )",
        )
        .bind(table)
        .fetch_one(app.pool())
        .await
        .unwrap();
        assert!(exists, "{table} is missing after migration");
    }

    // The columns 009 added.
    for column in ["cancelled_at", "cancel_reason"] {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM information_schema.columns
                 WHERE table_name = 'orders' AND column_name = $1
             )",
        )
        .bind(column)
        .fetch_one(app.pool())
        .await
        .unwrap();
        assert!(exists, "orders.{column} is missing");
    }
}

#[tokio::test]
async fn readiness_reports_a_live_pool_and_liveness_stays_dependency_free() {
    let app = TestApp::new().await;

    let (status, body) = app.get("/health/ready").await;
    assert_eq!(status, StatusCode::OK, "readiness said: {body}");
    assert_eq!(body["status"], "ready");
    assert_eq!(body["database"]["connected"], true);
    assert!(
        body["database"]["latency_ms"].is_u64(),
        "a successful check must report its latency: {body}"
    );
    assert!(
        body["database"]["pool_size"].as_u64().unwrap_or(0) >= 1,
        "the pool must hold at least the connection the check just used: {body}"
    );

    let (status, body) = app.get("/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn readiness_turns_503_when_the_pool_is_closed() {
    // The condition a load balancer must be able to see: the process is up, the database
    // is not. Closing the pool is the reachable stand-in for an unreachable server.
    let app = TestApp::new().await;
    assert_eq!(app.get("/health/ready").await.0, StatusCode::OK);

    app.storage.close().await;

    let (status, body) = app.get("/health/ready").await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a dead pool must not report ready: {body}"
    );
    assert_eq!(body["database"]["connected"], false);

    // Liveness must be unaffected: the process is still serving HTTP, and restarting it
    // would not bring the database back.
    let (status, body) = app.get("/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

// ---------------------------------------------------------------------------
// 2. Create — the row is really there
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_writes_the_order_and_its_cart_to_postgres() {
    let app = TestApp::new().await;

    let (status, body) = app.post("/api/orders", &order_body("T-01")).await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["status"], "open");
    assert_eq!(body["version"], 1);
    // 2×250 + 2×20 = 540, as a string so no JSON parser can turn a paisa into a float.
    assert_eq!(body["grand_total"], "540.00");

    // The response is not the evidence. These queries are.
    assert_eq!(app.order_count().await, 1, "the order row must exist");
    assert_eq!(app.order_item_count().await, 2, "both cart lines must exist");

    let row = app.order_row(body["id"].as_str().unwrap()).await;
    assert_eq!(row.customer_name, "Walk-in");
    assert_eq!(row.no_of_pax, 2);
    assert_eq!(row.restaurant_table.as_deref(), Some("T-01"));
    assert!(row.last_invoice.is_none());
    assert!(row.cancelled_at.is_none());

    // The server's computed total, in the column. NUMERIC compares by value, so scale
    // differences do not produce a false failure.
    assert_eq!(row.grand_total, rust_decimal_macros::dec!(540));
}

#[tokio::test]
async fn a_created_order_reads_back_over_http_with_its_lines() {
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;

    let (status, body) = app.get(&format!("/api/orders/{id}")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], id);
    assert_eq!(body["customer_name"], "Walk-in");
    assert_eq!(body["restaurant_table"], "T-01");
    assert_eq!(body["grand_total"], "540.00");

    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2);
    // Cart order is `order_items.idx`, which the repository writes 1-based in cart order.
    assert_eq!(items[0]["item"], "BIRYANI");
    assert_eq!(items[1]["item"], "TEA");
}

#[tokio::test]
async fn an_unknown_order_is_404_and_a_malformed_id_is_not_a_500() {
    let app = TestApp::new().await;

    // A well-formed id for a row that does not exist.
    let (status, body) = app.get("/api/orders/ORD-999999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["status"], 404);

    // Ids a client could only produce by inventing them. The surrogate key is an i64, so
    // these must not reach a parse panic or surface as a server fault.
    for bad in ["ORD-abc", "17", "ORD-", "ORD-0", "ORD--5"] {
        let (status, _) = app.get(&format!("/api/orders/{bad}")).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{bad:?} must be 404, not 400 or 500"
        );
    }
}

#[tokio::test]
async fn a_nonexistent_table_is_rejected_by_the_foreign_key_as_a_400() {
    // The in-memory store accepted any table name. The FK does not, and the caller can fix
    // it, so it must read as a 400 rather than a 500.
    let app = TestApp::new().await;

    let (status, body) = app
        .post(
            "/api/orders",
            &json!({"restaurant_table": "T-DOES-NOT-EXIST", "customer_name": "Walk-in"}),
        )
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body was {body}");
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("restaurant_table"),
        "the detail must name the offending field: {body}"
    );
    assert_eq!(app.order_count().await, 0, "nothing may have been written");
}

// ---------------------------------------------------------------------------
// 3. Idempotency — against a real key table
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ten_replays_of_one_key_insert_exactly_one_order() {
    let app = TestApp::new().await;
    let key = Uuid::new_v4().to_string();

    let mut ids = Vec::new();
    let mut statuses = Vec::new();
    for _ in 0..10 {
        let (status, body) = app
            .post_with_key("/api/orders", &order_body("T-01"), &key)
            .await;
        statuses.push(status);
        ids.push(body["id"].as_str().unwrap().to_owned());
    }

    assert_eq!(statuses[0], StatusCode::CREATED, "the first call creates");
    assert!(
        statuses[1..].iter().all(|s| *s == StatusCode::OK),
        "replays answer 200, not 201: {statuses:?}"
    );
    assert!(
        ids.windows(2).all(|w| w[0] == w[1]),
        "all ten replays must return one id: {ids:?}"
    );

    assert_eq!(app.order_count().await, 1, "one row, not ten");
    assert_eq!(app.order_item_count().await, 2, "one cart, not ten");

    let keys: i64 = sqlx::query_scalar("SELECT count(*) FROM order_idempotency_keys")
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(keys, 1, "one key row");
}

#[tokio::test]
async fn concurrent_replays_of_one_key_still_insert_one_order() {
    // The race the key table's primary key exists to lose: both requests miss the lookup,
    // both insert, one rolls back — order row included — and its retry finds the winner.
    let app = TestApp::new().await;
    let key = Uuid::new_v4().to_string();

    let mut handles = Vec::new();
    for _ in 0..8 {
        let router = app.app.clone();
        let key = key.clone();
        handles.push(tokio::spawn(async move {
            let request = axum::http::Request::builder()
                .method("POST")
                .uri("/api/orders")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header("idempotency-key", key)
                .body(axum::body::Body::from(
                    serde_json::to_vec(&order_body("T-01")).unwrap(),
                ))
                .unwrap();
            let response = tower::ServiceExt::oneshot(router, request).await.unwrap();
            let bytes = http_body_util::BodyExt::collect(response.into_body())
                .await
                .unwrap()
                .to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            json["id"].as_str().unwrap_or_default().to_owned()
        }));
    }

    let mut ids = Vec::new();
    for h in handles {
        ids.push(h.await.unwrap());
    }

    assert_eq!(app.order_count().await, 1, "exactly one row was committed");
    assert!(
        ids.iter().all(|id| !id.is_empty()),
        "every concurrent replay must get an answer: {ids:?}"
    );
    assert!(
        ids.windows(2).all(|w| w[0] == w[1]),
        "all replays must see one id: {ids:?}"
    );
}

#[tokio::test]
async fn distinct_keys_insert_distinct_orders_and_no_key_always_inserts() {
    let app = TestApp::new().await;

    let (_, first) = app
        .post_with_key(
            "/api/orders",
            &order_body("T-01"),
            &Uuid::new_v4().to_string(),
        )
        .await;
    let (_, second) = app
        .post_with_key(
            "/api/orders",
            &order_body("T-02"),
            &Uuid::new_v4().to_string(),
        )
        .await;
    assert_ne!(first["id"], second["id"]);
    assert_eq!(app.order_count().await, 2);

    // No key means no replay protection, which is what the client asked for.
    for table in ["T-03", "T-04"] {
        let (status, _) = app.post("/api/orders", &order_body(table)).await;
        assert_eq!(status, StatusCode::CREATED);
    }
    assert_eq!(app.order_count().await, 4);
}

// ---------------------------------------------------------------------------
// 4. Patch — under the real row lock
// ---------------------------------------------------------------------------

#[tokio::test]
async fn patch_replaces_the_cart_in_postgres_and_recomputes_the_total() {
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;

    let (status, body) = app
        .patch(
            &format!("/api/orders/{id}"),
            &json!({"items": [{"item": "DOSA", "item_name": "Masala Dosa", "qty": 1, "rate": 80}]}),
        )
        .await;

    assert_eq!(status, StatusCode::OK, "patch said: {body}");
    assert_eq!(body["version"], 2, "an accepted write advances the version");
    assert_eq!(body["grand_total"], "80.00");
    assert_eq!(body["items"].as_array().unwrap().len(), 1);

    // The cart is replaced wholesale, so the two original lines must be gone.
    assert_eq!(app.order_item_count().await, 1);
    let row = app.order_row(&id).await;
    assert_eq!(row.version, 2);
    assert_eq!(row.grand_total, rust_decimal_macros::dec!(80));
}

#[tokio::test]
async fn concurrent_appends_to_one_order_all_land() {
    // The property `SELECT ... FOR UPDATE` buys: twelve waiters adding a round each, and no
    // round lost to a read-modify-write racing another. Without the lock these would
    // clobber one another and the final cart would be short.
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;

    let mut handles = Vec::new();
    for i in 0..12 {
        let router = app.app.clone();
        let id = id.clone();
        handles.push(tokio::spawn(async move {
            let body = json!({
                "append_items": [
                    {"item": "TEA", "item_name": format!("Round {i}"), "qty": 1, "rate": 20}
                ]
            });
            let request = axum::http::Request::builder()
                .method("PATCH")
                .uri(format!("/api/orders/{id}"))
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap();
            tower::ServiceExt::oneshot(router, request)
                .await
                .unwrap()
                .status()
        }));
    }

    for h in handles {
        assert_eq!(h.await.unwrap(), StatusCode::OK);
    }

    // 2 original lines + 12 appended.
    assert_eq!(
        app.order_item_count().await,
        14,
        "no append may be lost to a lost update"
    );

    let row = app.order_row(&id).await;
    assert_eq!(row.version, 13, "one version bump per accepted write");
    // 540 + 12×20 = 780.
    assert_eq!(row.grand_total, rust_decimal_macros::dec!(780));
}

#[tokio::test]
async fn a_stale_version_is_a_409_and_writes_nothing() {
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;

    let (status, body) = app
        .patch(
            &format!("/api/orders/{id}"),
            &json!({"no_of_pax": 6, "version": 99}),
        )
        .await;

    assert_eq!(status, StatusCode::CONFLICT, "body was {body}");

    let row = app.order_row(&id).await;
    assert_eq!(row.version, 1, "the rejected write must not bump the version");
    assert_eq!(row.no_of_pax, 2, "and must not change the row");
}

#[tokio::test]
async fn a_patch_refused_by_validation_leaves_the_row_untouched() {
    // The mutation closure runs inside the transaction. A refusal must roll back, not
    // leave a half-applied cart behind.
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;

    let (status, _) = app
        .patch(
            &format!("/api/orders/{id}"),
            &json!({"items": [{"item": "TEA", "item_name": "Tea", "qty": 0, "rate": 20}]}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "qty 0 must be rejected");

    let row = app.order_row(&id).await;
    assert_eq!(row.version, 1);
    assert_eq!(app.order_item_count().await, 2, "the original cart survives");
}

// ---------------------------------------------------------------------------
// 5. Invoicing — gapless, against a Postgres counter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn invoicing_writes_an_invoice_and_links_the_order() {
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;

    let (status, body) = app
        .post(&format!("/api/orders/{id}/invoice"), &invoice_body())
        .await;

    assert_eq!(status, StatusCode::CREATED, "invoice said: {body}");

    let name = body["invoice_name"].as_str().expect("invoice_name");
    assert_eq!(name, "PCK-2627-000001", "first number in the series");
    assert!(
        name.chars().count() <= 16,
        "CGST Rule 46(b) caps the name at 16 characters: {name:?}"
    );
    assert_eq!(body["grand_total"], "540.00");
    assert_eq!(body["rounded_total"], "540.00");
    assert_eq!(body["round_off"], "0.00");

    assert_eq!(app.invoice_count().await, 1);
    assert_eq!(app.invoice_names().await, vec!["PCK-2627-000001"]);

    // The link, and the status derived from it.
    let row = app.order_row(&id).await;
    assert_eq!(row.last_invoice.as_deref(), Some("PCK-2627-000001"));

    let (_, order) = app.get(&format!("/api/orders/{id}")).await;
    assert_eq!(order["status"], "invoiced");
    assert_eq!(order["last_invoice"], "PCK-2627-000001");

    // The lines came across.
    let lines: i64 = sqlx::query_scalar("SELECT count(*) FROM invoice_lines WHERE invoice = $1")
        .bind(name)
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(lines, 2);
}

#[tokio::test]
async fn five_orders_produce_a_gapless_series() {
    let app = TestApp::new().await;

    let mut names = Vec::new();
    for table in ["T-01", "T-02", "T-03", "T-04", "T-05"] {
        let id = create_order(&app, table).await;
        let (status, body) = app
            .post(&format!("/api/orders/{id}/invoice"), &invoice_body())
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        names.push(body["invoice_name"].as_str().unwrap().to_owned());
    }

    assert_eq!(
        names,
        vec![
            "PCK-2627-000001",
            "PCK-2627-000002",
            "PCK-2627-000003",
            "PCK-2627-000004",
            "PCK-2627-000005",
        ]
    );

    // The database's own audit, not ours: `find_series_gaps` generates the range and looks
    // for holes.
    assert_eq!(app.issued_numbers().await, vec![1, 2, 3, 4, 5]);
    assert!(
        app.series_gaps().await.is_empty(),
        "Rule 46(b) forbids a gap"
    );
}

#[tokio::test]
async fn invoicing_the_same_order_twice_returns_the_first_number() {
    // The replay that matters most: no key, so the guard is the order's own `last_invoice`.
    // A second allocation here would gap the series.
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;

    let (first_status, first) = app
        .post(&format!("/api/orders/{id}/invoice"), &invoice_body())
        .await;
    let (second_status, second) = app
        .post(&format!("/api/orders/{id}/invoice"), &invoice_body())
        .await;

    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(second_status, StatusCode::OK, "a replay answers 200");
    assert_eq!(
        first["invoice_name"], second["invoice_name"],
        "no second number may be burned"
    );

    assert_eq!(app.invoice_count().await, 1);
    assert_eq!(app.issued_numbers().await, vec![1]);
}

#[tokio::test]
async fn an_idempotency_key_replay_returns_the_same_invoice() {
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;
    let key = Uuid::new_v4().to_string();

    let mut names = Vec::new();
    for _ in 0..5 {
        let (status, body) = app
            .post_with_key(&format!("/api/orders/{id}/invoice"), &invoice_body(), &key)
            .await;
        assert!(
            status == StatusCode::CREATED || status == StatusCode::OK,
            "unexpected {status}: {body}"
        );
        names.push(body["invoice_name"].as_str().unwrap().to_owned());
    }

    assert!(names.windows(2).all(|w| w[0] == w[1]), "one name: {names:?}");
    assert_eq!(app.invoice_count().await, 1);
    assert!(app.series_gaps().await.is_empty());
}

#[tokio::test]
async fn concurrent_invoicing_of_distinct_orders_stays_gapless() {
    // 5 tills billing at once against one Postgres counter. The row lock inside
    // `allocate_number` is what serialises them; nothing here retries.
    let app = TestApp::new().await;

    let mut ids = Vec::new();
    for table in ["T-01", "T-02", "T-03", "T-04", "T-05"] {
        ids.push(create_order(&app, table).await);
    }

    let mut handles = Vec::new();
    for id in ids {
        let router = app.app.clone();
        handles.push(tokio::spawn(async move {
            let request = axum::http::Request::builder()
                .method("POST")
                .uri(format!("/api/orders/{id}/invoice"))
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_vec(&invoice_body()).unwrap(),
                ))
                .unwrap();
            let response = tower::ServiceExt::oneshot(router, request).await.unwrap();
            let status = response.status();
            let bytes = http_body_util::BodyExt::collect(response.into_body())
                .await
                .unwrap()
                .to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            (status, json["invoice_name"].as_str().unwrap_or("").to_owned())
        }));
    }

    let mut names = Vec::new();
    for h in handles {
        let (status, name) = h.await.unwrap();
        assert_eq!(status, StatusCode::CREATED, "concurrent invoice failed");
        names.push(name);
    }

    names.sort();
    let expected: Vec<String> = (1..=5).map(|n| format!("PCK-2627-{n:06}")).collect();
    assert_eq!(names, expected, "no gaps, no duplicates");
    assert!(app.series_gaps().await.is_empty());
}

#[tokio::test]
async fn concurrent_invoicing_of_one_order_allocates_exactly_one_number() {
    // The lost update that would gap the series: eight requests, one order. The
    // `FOR UPDATE` on the order row is taken before the counter, so the losers block and
    // then see `last_invoice` already set.
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;

    let mut handles = Vec::new();
    for _ in 0..8 {
        let router = app.app.clone();
        let id = id.clone();
        handles.push(tokio::spawn(async move {
            let request = axum::http::Request::builder()
                .method("POST")
                .uri(format!("/api/orders/{id}/invoice"))
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_vec(&invoice_body()).unwrap(),
                ))
                .unwrap();
            let response = tower::ServiceExt::oneshot(router, request).await.unwrap();
            let bytes = http_body_util::BodyExt::collect(response.into_body())
                .await
                .unwrap()
                .to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            json["invoice_name"].as_str().unwrap_or("").to_owned()
        }));
    }

    let mut names = Vec::new();
    for h in handles {
        names.push(h.await.unwrap());
    }

    assert_eq!(app.invoice_count().await, 1, "exactly one invoice");
    assert_eq!(app.issued_numbers().await, vec![1], "exactly one number");
    assert!(
        names.windows(2).all(|w| w[0] == w[1]),
        "every caller must see the same name: {names:?}"
    );
}

#[tokio::test]
async fn a_rejected_invoice_burns_no_number() {
    // The no-burn guarantee, end to end. An over-long series is refused *after* the counter
    // has been incremented inside the transaction, so only the rollback keeps the series
    // intact — and the next legitimate invoice must still be number 1.
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;

    let mut body = invoice_body();
    body["series"] = json!("WAY-TOO-LONG-SERIES");
    let (status, _) = app
        .post(&format!("/api/orders/{id}/invoice"), &body)
        .await;
    assert_ne!(
        status,
        StatusCode::CREATED,
        "a name past 16 characters must be refused"
    );
    assert_eq!(app.invoice_count().await, 0);

    let (status, ok) = app
        .post(&format!("/api/orders/{id}/invoice"), &invoice_body())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{ok}");
    assert_eq!(
        ok["invoice_name"], "PCK-2627-000001",
        "the failed attempt must not have consumed number 1"
    );
    assert!(app.series_gaps().await.is_empty());
}

#[tokio::test]
async fn an_unregistered_series_is_refused_without_touching_the_order() {
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;

    let mut body = invoice_body();
    body["series"] = json!("NOPE");
    let (status, response) = app
        .post(&format!("/api/orders/{id}/invoice"), &body)
        .await;

    assert_ne!(status, StatusCode::CREATED, "{response}");
    assert_eq!(app.invoice_count().await, 0);
    assert!(
        app.order_row(&id).await.last_invoice.is_none(),
        "the order must not point at an invoice that was never written"
    );
}

#[tokio::test]
async fn an_empty_order_cannot_be_invoiced() {
    let app = TestApp::new().await;
    let (status, created) = app
        .post(
            "/api/orders",
            &json!({"restaurant_table": "T-02", "customer_name": "Walk-in"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap();

    let (status, _) = app
        .post(&format!("/api/orders/{id}/invoice"), &invoice_body())
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(app.invoice_count().await, 0);
    assert!(app.series_gaps().await.is_empty());
}

#[tokio::test]
async fn invoicing_rounds_to_the_rupee_and_records_the_residual() {
    // The round-off ledger's invariant, through the whole stack: 3 × 33.33 = 99.99 → 100
    // with a 0.01 residual, and the schema's `invoices_round_off_is_exact` CHECK would
    // reject the row if the arithmetic and the columns disagreed.
    let app = TestApp::new().await;

    let (status, created) = app
        .post(
            "/api/orders",
            &json!({
                "restaurant_table": "T-01",
                "customer_name": "Walk-in",
                "items": [{"item": "TEA", "item_name": "Masala Tea", "qty": 3, "rate": "33.33"}]
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap();

    let (status, body) = app
        .post(&format!("/api/orders/{id}/invoice"), &invoice_body())
        .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["grand_total"], "99.99");
    assert_eq!(body["rounded_total"], "100.00");
    assert_eq!(body["round_off"], "0.01");

    let (grand, rounded, round_off): (
        rust_decimal::Decimal,
        rust_decimal::Decimal,
        rust_decimal::Decimal,
    ) = sqlx::query_as(
        "SELECT grand_total, rounded_total, round_off FROM invoices WHERE name = $1",
    )
    .bind(body["invoice_name"].as_str().unwrap())
    .fetch_one(app.pool())
    .await
    .unwrap();

    assert_eq!(grand, rust_decimal_macros::dec!(99.99));
    assert_eq!(rounded, rust_decimal_macros::dec!(100));
    assert_eq!(round_off, rust_decimal_macros::dec!(0.01));
    assert_eq!(
        rounded - grand,
        round_off,
        "the invariant the round-off ledger account depends on"
    );
}

#[tokio::test]
async fn an_invoiced_order_cannot_be_modified() {
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;
    app.post(&format!("/api/orders/{id}/invoice"), &invoice_body())
        .await;

    let (status, _) = app
        .patch(&format!("/api/orders/{id}"), &json!({"no_of_pax": 8}))
        .await;

    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "once the invoice exists and the KOTs are printed, a silent line edit is not on"
    );
    assert_eq!(app.order_row(&id).await.no_of_pax, 2);
}

// ---------------------------------------------------------------------------
// 6. Cancellation — soft, audited, idempotent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancel_is_a_soft_delete_that_keeps_the_row() {
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;

    let (status, body) = app.delete(&format!("/api/orders/{id}")).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "cancelled");
    assert_eq!(body["version"], 2);

    // The row stays for the audit trail — this is not a DELETE.
    assert_eq!(app.order_count().await, 1);
    let row = app.order_row(&id).await;
    assert!(
        row.cancelled_at.is_some(),
        "cancellation must be stamped in the database"
    );
    assert_eq!(row.version, 2);

    let (status, read) = app.get(&format!("/api/orders/{id}")).await;
    assert_eq!(status, StatusCode::OK, "a cancelled order is still readable");
    assert_eq!(read["status"], "cancelled");
}

#[tokio::test]
async fn cancel_is_idempotent_and_blocks_later_edits() {
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;

    let (first, _) = app.delete(&format!("/api/orders/{id}")).await;
    let (second, body) = app.delete(&format!("/api/orders/{id}")).await;

    assert_eq!(first, StatusCode::OK);
    assert_eq!(second, StatusCode::OK, "a retried DELETE is not an error");
    assert_eq!(
        body["version"], 2,
        "the second cancel must not bump the version again"
    );

    let (status, _) = app
        .patch(&format!("/api/orders/{id}"), &json!({"no_of_pax": 4}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn cancelling_frees_the_table_for_a_new_order() {
    // The reason 009 had to rewrite `orders_one_live_form_per_table_idx`: with the original
    // unconditional index, the first cancelled order would hold T-01 forever.
    let app = TestApp::new().await;
    let first = create_order(&app, "T-01").await;
    app.delete(&format!("/api/orders/{first}")).await;

    let (status, body) = app.post("/api/orders", &order_body("T-01")).await;

    assert_eq!(
        status,
        StatusCode::CREATED,
        "a cancelled order must not hold its table forever: {body}"
    );
    assert_ne!(body["id"].as_str().unwrap(), first);
    assert_eq!(app.order_count().await, 2, "both rows exist");
}

#[tokio::test]
async fn an_invoiced_order_cannot_be_cancelled() {
    // Voiding a raised invoice is the invoice repository's business — it has its own
    // `cancel_reason` and its own Rule 46(b) audit trail. Cancelling only the form would
    // leave a billable invoice attached to an order the UI calls void.
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;
    app.post(&format!("/api/orders/{id}/invoice"), &invoice_body())
        .await;

    let (status, _) = app.delete(&format!("/api/orders/{id}")).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        app.order_row(&id).await.cancelled_at.is_none(),
        "the refused cancel must not have stamped the row"
    );
}

#[tokio::test]
async fn a_cancelled_order_cannot_be_invoiced() {
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;
    app.delete(&format!("/api/orders/{id}")).await;

    let (status, _) = app
        .post(&format!("/api/orders/{id}/invoice"), &invoice_body())
        .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(app.invoice_count().await, 0);
    assert!(app.series_gaps().await.is_empty());
}

// ---------------------------------------------------------------------------
// 7. Isolation and the end-to-end flow
// ---------------------------------------------------------------------------

#[tokio::test]
async fn state_survives_a_new_router_over_the_same_pool() {
    // The regression this lane closes: an in-memory store makes an order disappear when the
    // process is replaced. A second router over the same pool must still see it.
    let app = TestApp::new().await;
    let id = create_order(&app, "T-01").await;

    let second_router =
        peacock_api::build_with_storage(support::test_config(), app.storage.clone());
    let response = tower::ServiceExt::oneshot(
        second_router,
        axum::http::Request::builder()
            .uri(format!("/api/orders/{id}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the order must outlive the router that created it"
    );
}

#[tokio::test]
async fn the_full_flow_order_to_invoice_lands_in_postgres() {
    let app = TestApp::new().await;

    // Open a table.
    let id = create_order(&app, "T-01").await;

    // Add a round.
    let (status, _) = app
        .patch(
            &format!("/api/orders/{id}"),
            &json!({"append_items": [
                {"item": "DOSA", "item_name": "Masala Dosa", "qty": 1, "rate": 80}
            ]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // Bill it. 540 + 80 = 620.
    let (status, invoice) = app
        .post(&format!("/api/orders/{id}/invoice"), &invoice_body())
        .await;
    assert_eq!(status, StatusCode::CREATED, "{invoice}");
    assert_eq!(invoice["grand_total"], "620.00");

    let name = invoice["invoice_name"].as_str().unwrap();

    // Everything is where it should be, in the tables rather than in a response body.
    assert_eq!(app.order_count().await, 1);
    assert_eq!(app.order_item_count().await, 3);
    assert_eq!(app.invoice_count().await, 1);
    assert_eq!(app.order_row(&id).await.last_invoice.as_deref(), Some(name));

    let (customer, table, status_text, business_day): (
        String,
        Option<String>,
        String,
        chrono::NaiveDate,
    ) = sqlx::query_as(
        "SELECT customer, restaurant_table, status::TEXT, business_day
         FROM invoices WHERE name = $1",
    )
    .bind(name)
    .fetch_one(app.pool())
    .await
    .unwrap();

    assert_eq!(customer, "Walk-in");
    assert_eq!(table.as_deref(), Some("T-01"), "the table came across");
    assert_eq!(status_text, "Draft", "a fresh invoice is a draft");
    assert_eq!(business_day.to_string(), BUSINESS_DATE);

    // And the series the invoice was booked against is the one we asked for.
    let series: String = sqlx::query_scalar("SELECT naming_series FROM invoices WHERE name = $1")
        .bind(name)
        .fetch_one(app.pool())
        .await
        .unwrap();
    assert_eq!(series, SERIES);
}

#[tokio::test]
async fn two_test_databases_do_not_share_a_series_counter() {
    // Test isolation is a property the gapless assertions depend on: a shared counter would
    // make every "number 1" assertion depend on which test ran first.
    let a = TestApp::new().await;
    let b = TestApp::new().await;
    assert_ne!(a.db_name(), b.db_name());

    let id_a = create_order(&a, "T-01").await;
    let id_b = create_order(&b, "T-01").await;

    let (_, invoice_a) = a
        .post(&format!("/api/orders/{id_a}/invoice"), &invoice_body())
        .await;
    let (_, invoice_b) = b
        .post(&format!("/api/orders/{id_b}/invoice"), &invoice_body())
        .await;

    assert_eq!(invoice_a["invoice_name"], "PCK-2627-000001");
    assert_eq!(
        invoice_b["invoice_name"], "PCK-2627-000001",
        "each database has its own counter"
    );

    assert_eq!(a.invoice_count().await, 1);
    assert_eq!(b.invoice_count().await, 1);
}
