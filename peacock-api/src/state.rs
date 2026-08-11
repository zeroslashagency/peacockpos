//! Application state shared by every handler.
//!
//! Cheap to clone: Axum clones this per request, so everything expensive — the
//! connection pool above all — sits behind an `Arc`.
//!
//! # Storage is not optional (Lane W1-A)
//!
//! [`Inner::storage`] is a `Storage`, not optional, and there is exactly one
//! way to build a working state: [`AppState::with_storage`]. That is the whole point of
//! this lane. The previous shape held an optional `Storage` alongside an in-memory
//! `InvoiceStore` and an `InMemoryOrderStore`, and fell back to them when the option was
//! `None`. A POS that answers `201 Created` out of a `HashMap` is worse than one that
//! refuses to start: it hands the cashier a plausible invoice number, takes a shift's
//! takings, and loses all of it at the next restart. So the fallback is gone and the
//! type system now refuses to express it.
//!
//! Every repository accessor therefore returns the repository itself rather than an
//! `Option`, and no handler needs a "storage unavailable" branch. An unreachable database
//! surfaces where it actually happens — at the query — as a `StorageError` the handler
//! maps to 5xx, not as a silent switch onto a different implementation.

use std::sync::Arc;

use crate::config::Config;
use crate::events::EventBroadcaster;
use crate::routes::orders::KotRouting;
use crate::store::postgres_order::PostgresOrderStore;
use peacock_storage::Storage;

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    config: Config,
    /// Realtime fan-out (Lane 3H). Handlers publish here after a successful mutation;
    /// `/api/events/stream` subscribes. Per-`AppState` so each test observes only its
    /// own events.
    events: EventBroadcaster,
    /// The order store. Concrete, not a trait object: the Postgres repository is the only
    /// implementation there is now, and a one-implementor trait behind `async_trait` was
    /// costing a boxed future per call to describe a choice that no longer exists.
    orders: PostgresOrderStore,
    /// The four `peacock_core::kot` ports used when an order becomes an invoice
    /// (Lane 3D).
    kot_routing: KotRouting,
    /// The connection pool. Mandatory — see the module docs.
    storage: Storage,
}

impl AppState {
    /// The only way to build a state. Requires a live [`Storage`].
    pub fn with_storage(config: Config, storage: Storage) -> Self {
        Self::builder(config, storage).build()
    }

    /// Start building a state with non-default collaborators.
    ///
    /// Handlers only ever see the finished `AppState`, so this exists for wiring: the
    /// binary uses it to inject real repositories, tests to inject seeded routing. The
    /// storage argument is positional rather than a `with_` setter precisely so it cannot
    /// be forgotten.
    pub fn builder(config: Config, storage: Storage) -> AppStateBuilder {
        AppStateBuilder {
            config,
            storage,
            events: None,
            kot_routing: None,
        }
    }

    /// A state over a shared test database, for unit tests that only need *a* pool.
    ///
    /// `#[cfg(test)]`, so it exists only while the library's own unit tests are compiled:
    /// no production build and no integration test can reach it. It still hands back a
    /// real `Storage` over a real migrated database — the point of this lane is that
    /// nothing anywhere gets a fake one, tests included. Tests that assert on invoice
    /// numbering need an *isolated* database instead; those use
    /// [`crate::testing::TestDb`].
    #[cfg(test)]
    pub fn new(config: Config) -> Self {
        Self::with_storage(config, crate::testing::shared_storage())
    }

    /// State with an explicit event bus, over the shared test database.
    ///
    /// Tests use it to shrink the channel and replay capacities so subscriber lag and
    /// replay eviction are reachable without publishing thousands of events.
    #[cfg(test)]
    pub fn with_broadcaster(config: Config, events: EventBroadcaster) -> Self {
        Self::builder(config, crate::testing::shared_storage())
            .with_events(events)
            .build()
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    /// The order store shared by every handler on this state.
    pub fn orders(&self) -> &PostgresOrderStore {
        &self.inner.orders
    }

    /// The KOT routing ports used by `POST /api/orders/:id/invoice`.
    pub fn kot_repos(&self) -> &KotRouting {
        &self.inner.kot_routing
    }

    /// The realtime event bus. Cheap to clone; publish into it after a mutation commits.
    pub fn events(&self) -> &EventBroadcaster {
        &self.inner.events
    }

    /// The storage layer. Always present.
    pub fn storage(&self) -> &Storage {
        &self.inner.storage
    }

    /// Menu repository for KOT routing (courses per item).
    pub fn menu_repo(&self) -> peacock_storage::repos::PgMenuRepo {
        peacock_storage::repos::PgMenuRepo::new(self.storage().pool().clone())
    }

    /// Menu resolution repository (menu strategies, item lists), scoped to a restaurant.
    pub fn menu_resolution_repo(
        &self,
        restaurant: peacock_core::ids::RestaurantName,
    ) -> peacock_storage::repos::PgMenuResolutionRepo {
        peacock_storage::repos::PgMenuResolutionRepo::new(
            self.storage().pool().clone(),
            restaurant,
        )
    }

    /// Price repository for price lookups.
    pub fn price_repo(&self) -> peacock_storage::repos::PgPriceRepo {
        peacock_storage::repos::PgPriceRepo::new(self.storage().pool().clone())
    }

    /// Shift repository for shift management (Lane 4A-4).
    pub fn shift_repo(&self) -> peacock_storage::repos::PostgresShiftRepo {
        peacock_storage::repos::PostgresShiftRepo::new(self.storage().clone())
    }

    /// Table repository for table operations (Lane 4A-4).
    pub fn table_repo(&self) -> peacock_storage::repos::PostgresTableRepo {
        peacock_storage::repos::PostgresTableRepo::new(self.storage().pool().clone())
    }

    /// Invoice repository — gapless numbering, idempotency, payments.
    ///
    /// Returns the repository, not an `Option<_>`: there is no second backend to fall
    /// back to, so there is nothing for a caller to branch on. A database that is down
    /// fails the query, which is the truthful place for that failure to appear.
    pub fn invoice_repo(&self) -> peacock_storage::repos::PgInvoiceRepo {
        peacock_storage::repos::PgInvoiceRepo::new(self.storage().clone())
    }

    /// KOT repository — ticket creation, kitchen display, mark-prepared.
    pub fn kot_repo(&self) -> peacock_storage::repos::PgKotRepo {
        peacock_storage::repos::PgKotRepo::new(self.storage().clone())
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("config", &self.inner.config)
            .field("storage", &self.inner.storage)
            .finish()
    }
}

/// Assembles an [`AppState`] with chosen collaborators, defaulting the rest.
pub struct AppStateBuilder {
    config: Config,
    storage: Storage,
    events: Option<EventBroadcaster>,
    kot_routing: Option<KotRouting>,
}

impl AppStateBuilder {
    pub fn with_events(mut self, events: EventBroadcaster) -> Self {
        self.events = Some(events);
        self
    }

    pub fn with_kot_routing(mut self, routing: KotRouting) -> Self {
        self.kot_routing = Some(routing);
        self
    }

    pub fn build(self) -> AppState {
        // The order store follows the storage. There is no alternative to select: the
        // in-memory store this used to be able to pick is gone, so a state that connected
        // to Postgres and then wrote orders to a `HashMap` is no longer representable.
        let orders = PostgresOrderStore::new(self.storage.clone());

        AppState {
            inner: Arc::new(Inner {
                config: self.config,
                events: self.events.unwrap_or_default(),
                orders,
                kot_routing: self.kot_routing.unwrap_or_default(),
                storage: self.storage,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestDb;

    #[tokio::test]
    async fn clones_share_one_config() {
        let db = TestDb::new().await;
        let state = AppState::with_storage(Config::default(), db.storage().clone());
        let clone = state.clone();
        assert_eq!(
            state.config().bind_addr,
            clone.config().bind_addr,
            "clones must observe the same configuration"
        );
        assert!(Arc::ptr_eq(&state.inner, &clone.inner));
    }

    #[tokio::test]
    async fn clones_share_one_connection_pool() {
        // Axum clones the state per request. A per-clone pool would multiply the
        // configured connection ceiling by the request rate, and — for the invoice
        // counter — put concurrent allocations on pools that cannot see each other's
        // row locks. `Inner` is behind one `Arc`, so the pool is literally the same value
        // rather than a clone of it.
        let db = TestDb::new().await;
        let state = AppState::with_storage(Config::default(), db.storage().clone());
        let clone = state.clone();
        assert!(std::ptr::eq(state.storage().pool(), clone.storage().pool()));
    }

    #[tokio::test]
    async fn clones_share_one_event_bus() {
        // A per-clone bus would mean a handler publishing on one request clone is
        // invisible to the SSE connection held by another.
        let db = TestDb::new().await;
        let state = AppState::with_storage(Config::default(), db.storage().clone());
        let clone = state.clone();
        state
            .events()
            .publish(crate::events::EventKind::OrderCreated, serde_json::json!({}));
        assert_eq!(clone.events().last_event_id(), 1);
    }

    #[tokio::test]
    async fn separate_states_have_separate_event_buses() {
        let db = TestDb::new().await;
        let first = AppState::with_storage(Config::default(), db.storage().clone());
        let second = AppState::with_storage(Config::default(), db.storage().clone());
        first
            .events()
            .publish(crate::events::EventKind::OrderCreated, serde_json::json!({}));
        assert_eq!(second.events().last_event_id(), 0);
    }

    #[tokio::test]
    async fn an_order_written_through_one_clone_is_visible_through_another() {
        // The property the old `Arc<dyn OrderStore>` existed to guarantee, now a property
        // of the shared pool: both clones read the same rows.
        let db = TestDb::new().await;
        let state = AppState::with_storage(Config::default(), db.storage().clone());
        let clone = state.clone();

        let created = state
            .orders()
            .create(db.takeaway_form(), None)
            .await
            .unwrap()
            .0;
        assert!(
            clone.orders().get(&created.id).await.is_ok(),
            "an order created through one clone must be visible through another"
        );
    }

    #[tokio::test]
    async fn separate_databases_do_not_share_orders() {
        // Test isolation depends on this: one throwaway database per test, so a stray
        // row cannot make another test's assertion pass.
        let first_db = TestDb::new().await;
        let second_db = TestDb::new().await;
        let first = AppState::with_storage(Config::default(), first_db.storage().clone());
        let second = AppState::with_storage(Config::default(), second_db.storage().clone());

        let created = first
            .orders()
            .create(first_db.takeaway_form(), None)
            .await
            .unwrap()
            .0;
        assert!(
            second.orders().get(&created.id).await.is_err(),
            "test isolation depends on this"
        );
    }

    #[tokio::test]
    async fn the_builder_defaults_everything_not_supplied() {
        let db = TestDb::new().await;
        let state = AppState::builder(Config::default(), db.storage().clone()).build();
        assert_eq!(state.config().bind_addr.port(), 3000);
        assert_eq!(state.events().last_event_id(), 0);
    }

    #[tokio::test]
    async fn the_builder_installs_the_supplied_kot_routing() {
        let db = TestDb::new().await;
        let routing = KotRouting::new().with_unit("Hot Kitchen", "Peacock - Main", &["Main Course"]);
        let state = AppState::builder(Config::default(), db.storage().clone())
            .with_kot_routing(routing)
            .build();

        let units = peacock_core::ports::ProductionRepo::list_for_branch(
            state.kot_repos(),
            &peacock_core::ids::BranchName::from("Peacock - Main"),
        )
        .unwrap();
        assert_eq!(
            units.len(),
            1,
            "the handler-visible routing must be the one that was injected"
        );
    }

    #[tokio::test]
    async fn a_repository_accessor_answers_instead_of_panicking_on_a_missing_pool() {
        // The old accessors were `self.storage().expect("storage must be available")`, so
        // a state built without a database aborted the request mid-flight. There is no
        // such state now, and the accessor answers a real query.
        let db = TestDb::new().await;
        let state = AppState::with_storage(Config::default(), db.storage().clone());

        assert!(state
            .invoice_repo()
            .peek_series("NOPE", "2627")
            .await
            .unwrap()
            .is_none());
        // A registered series does have a counter, so the accessor is reading this
        // database and not some other one.
        assert_eq!(
            state.invoice_repo().peek_series("POS", "2627").await.unwrap(),
            Some(1)
        );
    }
}
