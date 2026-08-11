use peacock_core::ids::*;
use peacock_core::model::{Kot, KotItem, KotType};
use peacock_core::ports::KotRepo;
use peacock_storage::repos::PgKotRepo;
use peacock_storage::{DbConfig, Storage};
use rust_decimal_macros::dec;
use sqlx::types::chrono::{NaiveDate, NaiveTime};
use std::collections::HashSet;
use std::sync::Arc;

/// Setup test database with migrations
async fn setup_test_db() -> Storage {
    let config = DbConfig::from_env().expect("DATABASE_URL must be set");
    let storage = Storage::connect(config).await.expect("connect failed");
    storage.migrate().await.expect("migrations failed");
    storage
}

/// Sample KOT builder
fn sample_kot(naming_series: &str, invoice: &str, production: &str) -> Kot {
    Kot {
        name: None,
        naming_series: naming_series.to_owned(),
        invoice: invoice.to_owned(),
        restaurant_table: Some(TableName::from("T-01")),
        customer_name: Some(CustomerName::from("Walk-in")),
        original_kot: None,
        date: NaiveDate::from_ymd_opt(2026, 7, 28).unwrap(),
        time: Some(NaiveTime::from_hms_opt(12, 30, 0).unwrap()),
        kot_type: KotType::NewOrder,
        order_status: None,
        production: Some(ProductionUnitName::from(production)),
        start_time_prep: None,
        kot_items: vec![
            KotItem {
                item: ItemCode::from("CURRY"),
                item_name: "Chicken Curry".to_owned(),
                quantity: dec!(2),
                cancelled_qty: dec!(0),
                comments: None,
                course: Some(MenuCourseName::from("Main Course")),
                serve_priority: 0,
                indicate_course: false,
            },
            KotItem {
                item: ItemCode::from("NAAN"),
                item_name: "Butter Naan".to_owned(),
                quantity: dec!(4),
                cancelled_qty: dec!(0),
                comments: Some("Extra butter".to_owned()),
                course: Some(MenuCourseName::from("Breads")),
                serve_priority: 1,
                indicate_course: true,
            },
        ],
        pos_profile: Some(PosProfileName::from("Peacock POS")),
        branch: Some(BranchName::from("Peacock - Main")),
        verified: false,
        verified_by: None,
        table_takeaway: false,
        is_aggregator: false,
        aggregator_id: None,
        comments: None,
        order_no: Some("ORD-001".to_owned()),
    }
}

#[tokio::test]
async fn create_assigns_gapless_name() {
    let storage = setup_test_db().await;
    let repo = PgKotRepo::new(storage);

    let kot = sample_kot("KOT-", "INV-001", "Hot Kitchen");
    let created = repo.create(kot).await.expect("create failed");

    assert!(created.name.is_some(), "name must be assigned");
    assert!(
        created.name.as_ref().unwrap().as_str().starts_with("KOT-"),
        "name must use naming series prefix"
    );
}

#[tokio::test]
async fn create_preserves_all_fields() {
    let storage = setup_test_db().await;
    let repo = PgKotRepo::new(storage);

    let kot = sample_kot("KOT-", "INV-002", "Cold Kitchen");
    let created = repo.create(kot.clone()).await.expect("create failed");

    assert_eq!(created.naming_series, kot.naming_series);
    assert_eq!(created.invoice, kot.invoice);
    assert_eq!(created.restaurant_table, kot.restaurant_table);
    assert_eq!(created.customer_name, kot.customer_name);
    assert_eq!(created.date, kot.date);
    assert_eq!(created.time, kot.time);
    assert_eq!(created.kot_type, kot.kot_type);
    assert_eq!(created.production, kot.production);
    assert_eq!(created.kot_items.len(), kot.kot_items.len());
    assert_eq!(created.kot_items[0].item, kot.kot_items[0].item);
    assert_eq!(created.kot_items[1].item, kot.kot_items[1].item);
}

#[tokio::test]
async fn create_preserves_item_order() {
    let storage = setup_test_db().await;
    let repo = PgKotRepo::new(storage);

    let kot = sample_kot("KOT-", "INV-003", "Bar");
    let created = repo.create(kot.clone()).await.expect("create failed");

    // Fetch it back and verify order
    let fetched = repo.get(created.name.as_ref().unwrap()).await.expect("get failed");
    assert_eq!(fetched.kot_items.len(), 2);
    assert_eq!(fetched.kot_items[0].item.as_str(), "CURRY");
    assert_eq!(fetched.kot_items[1].item.as_str(), "NAAN");
}

#[tokio::test]
async fn get_fetches_kot_with_all_items() {
    let storage = setup_test_db().await;
    let repo = PgKotRepo::new(storage);

    let kot = sample_kot("KOT-", "INV-004", "Hot Kitchen");
    let created = repo.create(kot).await.expect("create failed");

    let fetched = repo.get(created.name.as_ref().unwrap()).await.expect("get failed");
    assert_eq!(fetched.name, created.name);
    assert_eq!(fetched.kot_items.len(), 2);
}

#[tokio::test]
async fn exists_for_returns_false_when_no_kot() {
    let storage = setup_test_db().await;
    let repo = PgKotRepo::new(storage);

    let exists = repo
        .exists_for("NONEXISTENT-INV", &ProductionUnitName::from("Hot Kitchen"))
        .expect("exists_for failed");
    assert!(!exists, "should return false for non-existent KOT");
}

#[tokio::test]
async fn exists_for_returns_true_after_creation() {
    let storage = setup_test_db().await;
    let repo = PgKotRepo::new(storage);

    let kot = sample_kot("KOT-", "INV-005", "Hot Kitchen");
    repo.create(kot).await.expect("create failed");

    let exists = repo
        .exists_for("INV-005", &ProductionUnitName::from("Hot Kitchen"))
        .expect("exists_for failed");
    assert!(exists, "should return true after creation");
}

#[tokio::test]
async fn exists_for_is_scoped_to_production_unit() {
    let storage = setup_test_db().await;
    let repo = PgKotRepo::new(storage);

    let kot = sample_kot("KOT-", "INV-006", "Hot Kitchen");
    repo.create(kot).await.expect("create failed");

    // Same invoice, different production unit
    let exists = repo
        .exists_for("INV-006", &ProductionUnitName::from("Cold Kitchen"))
        .expect("exists_for failed");
    assert!(!exists, "should be scoped to production unit");
}

#[tokio::test]
async fn concurrent_creates_produce_gapless_sequence() {
    let storage = setup_test_db().await;
    let storage = Arc::new(storage);

    // Create 100 KOTs concurrently
    let mut handles = vec![];
    for i in 0..100 {
        let storage = Arc::clone(&storage);
        let handle = tokio::spawn(async move {
            let repo = PgKotRepo::new((*storage).clone());
            let kot = sample_kot("KOT-", &format!("INV-CONC-{:03}", i), "Hot Kitchen");
            repo.create(kot).await.expect("create failed")
        });
        handles.push(handle);
    }

    let results: Vec<Kot> = {
        let mut all = Vec::new();
        for handle in handles {
            all.push(handle.await.expect("task panicked"));
        }
        all
    };

    // Extract sequence numbers from names
    let mut seq_numbers: Vec<u64> = results
        .iter()
        .map(|k| {
            let name = k.name.as_ref().unwrap().as_str();
            name.strip_prefix("KOT-")
                .unwrap()
                .parse::<u64>()
                .unwrap()
        })
        .collect();

    seq_numbers.sort_unstable();

    // Verify no gaps and no duplicates
    assert_eq!(seq_numbers.len(), 100, "should have 100 results");
    let unique: HashSet<_> = seq_numbers.iter().collect();
    assert_eq!(unique.len(), 100, "no duplicate numbers");

    // Check for gaps: each number should be exactly 1 more than the previous
    for window in seq_numbers.windows(2) {
        assert_eq!(
            window[1],
            window[0] + 1,
            "gap detected: {} -> {}",
            window[0],
            window[1]
        );
    }
}

#[tokio::test]
async fn list_items_batch_fetches_multiple_kots() {
    let storage = setup_test_db().await;
    let repo = PgKotRepo::new(storage);

    // Create 3 KOTs
    let kot1 = repo
        .create(sample_kot("KOT-", "INV-BATCH-1", "Hot Kitchen"))
        .await
        .expect("create failed");
    let kot2 = repo
        .create(sample_kot("KOT-", "INV-BATCH-2", "Cold Kitchen"))
        .await
        .expect("create failed");
    let kot3 = repo
        .create(sample_kot("KOT-", "INV-BATCH-3", "Bar"))
        .await
        .expect("create failed");

    let names = vec![
        kot1.name.unwrap(),
        kot2.name.unwrap(),
        kot3.name.unwrap(),
    ];

    let items_map = repo.list_items_batch(&names).await.expect("batch fetch failed");

    assert_eq!(items_map.len(), 3, "should fetch all 3 KOTs");
    for (_name, items) in items_map {
        assert_eq!(items.len(), 2, "each KOT has 2 items");
        assert_eq!(items[0].item.as_str(), "CURRY");
        assert_eq!(items[1].item.as_str(), "NAAN");
    }
}

#[tokio::test]
async fn list_items_batch_n_plus_1_fix() {
    let storage = setup_test_db().await;
    let repo = PgKotRepo::new(storage);

    // Create 12 KOTs (simulating 12 items × 3 stations scenario)
    let mut names = vec![];
    for i in 0..12 {
        let kot = repo
            .create(sample_kot("KOT-", &format!("INV-N1-{}", i), "Hot Kitchen"))
            .await
            .expect("create failed");
        names.push(kot.name.unwrap());
    }

    // Fetch all items in ONE query
    let items_map = repo.list_items_batch(&names).await.expect("batch fetch failed");

    // Verify we got all items
    assert_eq!(items_map.len(), 12);
    assert_eq!(
        items_map.values().map(|v| v.len()).sum::<usize>(),
        24,
        "12 KOTs × 2 items = 24 total"
    );
}

#[tokio::test]
async fn list_pending_for_production_filters_by_unit() {
    let storage = setup_test_db().await;
    let repo = PgKotRepo::new(storage);

    let date = NaiveDate::from_ymd_opt(2026, 7, 29).unwrap();

    // Create KOTs for different production units
    let mut hot = sample_kot("KOT-", "INV-PEND-1", "Hot Kitchen");
    hot.date = date;
    repo.create(hot).await.expect("create failed");

    let mut cold = sample_kot("KOT-", "INV-PEND-2", "Cold Kitchen");
    cold.date = date;
    repo.create(cold).await.expect("create failed");

    let mut bar = sample_kot("KOT-", "INV-PEND-3", "Bar");
    bar.date = date;
    repo.create(bar).await.expect("create failed");

    // Query for Hot Kitchen only
    let hot_kots = repo
        .list_pending_for_production(&ProductionUnitName::from("Hot Kitchen"), date, date)
        .await
        .expect("list failed");

    assert_eq!(hot_kots.len(), 1);
    assert_eq!(hot_kots[0].invoice, "INV-PEND-1");
}

#[tokio::test]
async fn list_pending_excludes_cancelled_kots() {
    let storage = setup_test_db().await;
    let repo = PgKotRepo::new(storage);

    let date = NaiveDate::from_ymd_opt(2026, 7, 30).unwrap();

    // Create a cancelled KOT
    let mut cancelled = sample_kot("CNCL-", "INV-CANCEL-1", "Hot Kitchen");
    cancelled.date = date;
    cancelled.kot_type = KotType::Cancelled;
    repo.create(cancelled).await.expect("create failed");

    // Create a pending KOT
    let mut pending = sample_kot("KOT-", "INV-CANCEL-2", "Hot Kitchen");
    pending.date = date;
    repo.create(pending).await.expect("create failed");

    let kots = repo
        .list_pending_for_production(&ProductionUnitName::from("Hot Kitchen"), date, date)
        .await
        .expect("list failed");

    assert_eq!(kots.len(), 1, "should only return pending KOTs");
    assert_eq!(kots[0].invoice, "INV-CANCEL-2");
}

#[tokio::test]
async fn create_rejects_kot_with_existing_name() {
    let storage = setup_test_db().await;
    let repo = PgKotRepo::new(storage);

    let mut kot = sample_kot("KOT-", "INV-DUP", "Hot Kitchen");
    kot.name = Some(KotName::from("KOT-00042"));

    let result = repo.create(kot).await;
    assert!(result.is_err(), "should reject KOT with pre-set name");
}

#[tokio::test]
async fn kot_items_preserve_decimal_precision() {
    let storage = setup_test_db().await;
    let repo = PgKotRepo::new(storage);

    let mut kot = sample_kot("KOT-", "INV-DEC", "Hot Kitchen");
    kot.kot_items[0].quantity = dec!(2.5);
    kot.kot_items[0].cancelled_qty = dec!(0.25);

    let created = repo.create(kot).await.expect("create failed");
    let fetched = repo.get(created.name.as_ref().unwrap()).await.expect("get failed");

    assert_eq!(fetched.kot_items[0].quantity, dec!(2.5));
    assert_eq!(fetched.kot_items[0].cancelled_qty, dec!(0.25));
}

#[tokio::test]
async fn kot_supports_all_four_types() {
    let storage = setup_test_db().await;
    let repo = PgKotRepo::new(storage);

    let types = [
        KotType::NewOrder,
        KotType::OrderModified,
        KotType::Cancelled,
        KotType::PartiallyCancelled,
    ];

    for (i, kot_type) in types.iter().enumerate() {
        let mut kot = sample_kot("KOT-", &format!("INV-TYPE-{}", i), "Hot Kitchen");
        kot.kot_type = *kot_type;
        let created = repo.create(kot).await.expect("create failed");
        let fetched = repo.get(created.name.as_ref().unwrap()).await.expect("get failed");
        assert_eq!(fetched.kot_type, *kot_type);
    }
}
