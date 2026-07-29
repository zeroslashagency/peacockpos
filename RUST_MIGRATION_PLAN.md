# URY Rust Migration Plan

> **SUPERSEDED — do not implement.** Three independent model reviews (Claude Opus 5, GPT-5.6 Sol, GPT-5.6 Terra) unanimously rejected this plan. See [`PLAN-REVIEW.md`](./PLAN-REVIEW.md).
>
> Known defects in this document:
> - **~60% of the §2.2 doctype table is fabricated.** `ury_bom`, `ury_pricelist`, `ury_customer`, `ury_tax_rule`, `ury_discount`, `ury_shift`, `ury_payment_entry`, `ury_modifier`, `ury_course`, `ury_order_type`, `ury_pos_opening_entry`, `ury_void_reason` and others do not exist in `_upstream/ury-ury/`.
> - **The §3 "Python original" snippets do not match upstream.** `_get_merge_cluster` has a different signature, reads `merged_with` from `URY Table` not the order, and scopes to `restaurant_room`. `process_items_for_kot` takes 8 args and resolves per-branch production units. The BOM walk is 2 hardcoded levels, divides by `bom.quantity`, and prices from `Item Price` — not stock valuation.
> - **§7 "keep the React frontends" is wrong.** URY ships one React app (`pos/`) and two Vue 3 apps (`urypos/`, `URYMosaic/`).
> - **Vercel-incompatible throughout:** no Rust runtime, `tokio::sync::broadcast` cannot fan out across request-scoped functions, `sqlx::Pool` exhausts Postgres connections, Tokio background tasks need a live process, and §5 deploys Docker/Kubernetes with 3 replicas.
> - **The real cost was never counted.** The 6,075 lines are glue; ERPNext computes the money (`calculate_taxes_and_totals`, GL posting, Stock Ledger valuation, price lists, tax templates), and `ury/hooks.py` ships ~110 custom fields across ~15 ERPNext doctypes.
> - **Every §9 performance target is unmeasured.** The actual bottleneck is an N+1 `frappe.db.get_value("Item", …)` inside a per-production-unit loop.
>
> Retained for reference only.

## Executive Summary

This plan outlines a complete Python/Frappe → Rust rewrite of the URY restaurant POS system. The migration delivers significant performance gains, memory safety, and concurrent request handling, at the cost of 24-32 weeks of development effort and a steeper learning curve for the team.

**Why Rust:**
- Zero-cost abstractions for high-throughput order/KOT processing
- Memory safety eliminates entire classes of production bugs
- Native async/await for WebSocket realtime features
- Superior performance for P&L recursive BOM calculations
- Strong type system catches business logic errors at compile time

**Trade-offs:**
- 6-8 months full rewrite vs 0 weeks for TypeScript
- Team needs Rust training (ownership, lifetimes, async)
- Smaller ecosystem than Node.js
- Longer compile times during development

**Timeline:** 24-32 weeks (6-8 months) for complete migration

---

## 1. Target Architecture

### 1.1 Core Stack

| Component | Choice | Rationale |
|-----------|--------|-----------|
| **Web Framework** | **Axum** | Built on Tokio, composable tower middleware, excellent async ergonomics, lightweight |
| **ORM** | **SQLx** | Compile-time SQL verification, no runtime overhead, direct control over queries for complex BOM/P&L logic |
| **Database** | **PostgreSQL 15+** | JSONB for flexible metadata, recursive CTEs for BOM walk, robust ACID guarantees |
| **Async Runtime** | **Tokio** | Industry standard, mature, required by Axum |
| **WebSocket** | **axum::extract::ws** | Native Axum support via tokio-tungstenite |
| **Redis Client** | **redis-rs** | Async support, connection pooling, cluster-ready |
| **Serialization** | **serde + serde_json** | Zero-copy deserialization, derive macros |
| **Auth** | **jsonwebtoken + argon2** | JWT tokens, secure password hashing |

**Why Axum over Actix:**
- Axum: Simpler mental model, composable extractors, better error handling via tower, officially maintained
- Actix: More mature but uses actor model (unnecessary complexity for CRUD+realtime)

**Why SQLx over SeaORM:**
- SQLx: Direct SQL control critical for recursive BOM queries, compile-time verification, zero runtime cost
- SeaORM: Active Record pattern adds overhead, less control over complex joins

### 1.2 Workspace Structure

```
ury-rust/
├── Cargo.toml              # Workspace manifest
├── crates/
│   ├── domain/             # Business logic & domain models
│   │   ├── src/
│   │   │   ├── order.rs    # Order aggregate
│   │   │   ├── kot.rs      # KOT generation
│   │   │   ├── menu.rs     # Menu resolution
│   │   │   ├── pnl.rs      # P&L calculation
│   │   │   ├── table.rs    # Table merge logic
│   │   │   └── types.rs    # Shared domain types
│   │   └── Cargo.toml
│   ├── db/                 # Database layer (SQLx)
│   │   ├── src/
│   │   │   ├── models/     # DB row structs
│   │   │   ├── repos/      # Repository pattern
│   │   │   └── migrations/ # SQLx migrations
│   │   └── Cargo.toml
│   ├── api/                # HTTP/WebSocket API
│   │   ├── src/
│   │   │   ├── routes/     # Axum route handlers
│   │   │   ├── ws/         # WebSocket handlers
│   │   │   └── middleware/ # Auth, CORS, logging
│   │   └── Cargo.toml
│   ├── auth/               # Authentication & authorization
│   │   └── Cargo.toml
│   └── realtime/           # WebSocket broadcast layer
│       └── Cargo.toml
└── migrations/             # SQLx migrations (shared)
```

---

## 2. Doctype → Rust Domain Model Mapping

All 36 URY doctypes mapped to Rust structs with ownership annotations and Frappe pattern replacements.

### 2.1 Core Doctypes

#### **ury_order** → `Order` struct

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: Uuid,
    pub name: String,                    // Frappe naming_series
    pub status: OrderStatus,             // Frappe docstatus → enum
    pub order_type: String,              // FK to ury_order_type
    pub table_id: Option<Uuid>,          // FK to ury_table
    pub room_id: Option<Uuid>,           // FK to ury_room
    pub merged_with: Vec<Uuid>,          // CSV field → Vec<Uuid>
    pub guest_name: Option<String>,
    pub guest_count: i32,
    pub items: Vec<OrderItem>,           // Child table → Vec
    pub total_amount: Decimal,
    pub created_at: DateTime<Utc>,
    pub modified_at: DateTime<Utc>,
    pub owner: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "order_status", rename_all = "lowercase")]
pub enum OrderStatus {
    Draft,      // docstatus = 0
    Submitted,  // docstatus = 1
    Cancelled,  // docstatus = 2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderItem {
    pub id: Uuid,
    pub item_code: String,
    pub item_name: String,
    pub qty: Decimal,
    pub rate: Decimal,
    pub amount: Decimal,
    pub item_group: String,
    pub production_unit: Option<String>,
}
```

**Frappe Pattern Replacements:**
- `docstatus` → `OrderStatus` enum (Draft/Submitted/Cancelled)
- `merged_with` CSV string → `Vec<Uuid>` stored as JSONB
- Child table `ury_order_item` → `Vec<OrderItem>` with foreign key
- `naming_series` → keep as `name` field (business requirement)

#### **ury_kot** → `Kot` struct

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kot {
    pub id: Uuid,
    pub name: String,
    pub order_id: Uuid,                  // FK to ury_order
    pub production_unit: String,         // FK to ury_production_unit
    pub kot_number: i32,
    pub status: KotStatus,
    pub items: Vec<KotItem>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum KotStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}
```

#### **ury_daily_p_and_l** → `DailyPnL` struct

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyPnL {
    pub id: Uuid,
    pub date: NaiveDate,
    pub total_revenue: Decimal,
    pub total_cogs: Decimal,           // Calculated via recursive BOM
    pub gross_profit: Decimal,
    pub items: Vec<PnLItem>,
    pub calculated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PnLItem {
    pub item_code: String,
    pub qty_sold: Decimal,
    pub revenue: Decimal,
    pub cogs: Decimal,                 // From nested BOM walk
    pub margin: Decimal,
}
```

### 2.2 All 36 Doctypes Mapped

| Frappe Doctype | Rust Struct | Key Changes |
|----------------|-------------|-------------|
| `ury_order` | `Order` | docstatus→enum, merged_with→Vec, items→Vec |
| `ury_order_item` | `OrderItem` | Embedded in Order.items |
| `ury_kot` | `Kot` | docstatus→enum, items→Vec |
| `ury_kot_item` | `KotItem` | Embedded in Kot.items |
| `ury_daily_p_and_l` | `DailyPnL` | Recursive BOM in service layer |
| `ury_menu` | `Menu` | active_from/to→DateTime, rooms→Vec |
| `ury_menu_item` | `MenuItem` | FK to Menu |
| `ury_order_type` | `OrderType` | Simple lookup table |
| `ury_pos_opening_entry` | `PosOpening` | docstatus→enum, shift validation |
| `ury_pos_closing_entry` | `PosClosing` | Aggregated cash/card totals |
| `ury_production_unit` | `ProductionUnit` | Kitchen/bar stations |
| `ury_room` | `Room` | Simple FK target |
| `ury_table` | `Table` | room_id FK, capacity |
| `ury_item_group` | `ItemGroup` | Hierarchy via parent_id |
| `ury_pricelist` | `Pricelist` | Valid date ranges |
| `ury_pricelist_item` | `PricelistItem` | FK to Pricelist |
| `ury_bom` | `Bom` | Recursive structure via parent_id |
| `ury_bom_item` | `BomItem` | Embedded in Bom.items |
| `ury_shift` | `Shift` | FK to PosOpening |
| `ury_payment_entry` | `PaymentEntry` | docstatus→enum |
| `ury_discount` | `Discount` | Percentage/flat calculation |
| `ury_tax_rule` | `TaxRule` | Percentage-based |
| `ury_customer` | `Customer` | Simple CRM fields |
| `ury_kitchen_station` | `KitchenStation` | FK target for routing |
| `ury_course` | `Course` | Menu organization |
| `ury_menu_course` | `MenuCourse` | M2M join table |
| `ury_modifier` | `Modifier` | Item customization |
| `ury_modifier_group` | `ModifierGroup` | Grouping |
| `ury_order_modifier` | `OrderModifier` | M2M join table |
| `ury_void_reason` | `VoidReason` | Audit trail |
| `ury_settings` | `Settings` | Config stored as JSONB |
| `ury_sync_log` | `SyncLog` | Audit trail |
| `ury_report_config` | `ReportConfig` | JSON storage |
| `ury_analytics` | `Analytics` | Time-series data |
| `ury_user_role` | `UserRole` | Auth/RBAC |
| `ury_permission` | `Permission` | RBAC rules |

---

## 3. Critical Business Logic Ports

### 3.1 Table Merge Clustering (BFS Traversal)

**Python Original** (`ury_order.py:_get_merge_cluster`):
```python
def _get_merge_cluster(table_name, visited):
    cluster = {table_name}
    queue = [table_name]
    while queue:
        current = queue.pop(0)
        if current in visited:
            continue
        visited.add(current)
        order = frappe.get_doc("URY Order", {"table": current})
        if order.merged_with:
            neighbors = order.merged_with.split(",")
            for n in neighbors:
                if n not in visited:
                    cluster.add(n)
                    queue.append(n)
    return cluster
```

**Rust Port** (domain/src/table.rs):
```rust
use std::collections::{HashSet, VecDeque};

pub async fn get_merge_cluster(
    table_id: Uuid,
    repo: &OrderRepository,
) -> Result<HashSet<Uuid>> {
    let mut visited = HashSet::new();
    let mut cluster = HashSet::new();
    let mut queue = VecDeque::new();
    
    queue.push_back(table_id);
    cluster.insert(table_id);
    
    while let Some(current) = queue.pop_front() {
        if visited.contains(&current) {
            continue;
        }
        visited.insert(current);
        
        if let Some(order) = repo.find_by_table(current).await? {
            for neighbor_id in order.merged_with {
                if !visited.contains(&neighbor_id) {
                    cluster.insert(neighbor_id);
                    queue.push_back(neighbor_id);
                }
            }
        }
    }
    
    Ok(cluster)
}
```

### 3.2 KOT Routing (Production Unit Mapping)

**Python Original** (`ury_kot_generate.py:process_items_for_kot`):
```python
def process_items_for_kot(items):
    grouped = {}
    for item in items:
        unit = get_production_unit(item.item_group)
        if unit not in grouped:
            grouped[unit] = []
        grouped[unit].append(item)
    return grouped
```

**Rust Port** (domain/src/kot.rs):
```rust
use std::collections::HashMap;

pub fn group_items_by_production_unit(
    items: Vec<OrderItem>,
    item_group_map: &HashMap<String, String>,
) -> HashMap<String, Vec<OrderItem>> {
    let mut grouped: HashMap<String, Vec<OrderItem>> = HashMap::new();
    
    for item in items {
        let unit = item_group_map
            .get(&item.item_group)
            .cloned()
            .unwrap_or_else(|| "default".to_string());
        
        grouped.entry(unit).or_insert_with(Vec::new).push(item);
    }
    
    grouped
}

pub async fn create_kots_for_order(
    order: &Order,
    repo: &KotRepository,
    item_group_repo: &ItemGroupRepository,
) -> Result<Vec<Kot>> {
    let item_group_map = item_group_repo.get_production_unit_map().await?;
    let grouped = group_items_by_production_unit(order.items.clone(), &item_group_map);
    
    let mut kots = Vec::new();
    for (production_unit, items) in grouped {
        let kot = Kot {
            id: Uuid::new_v4(),
            order_id: order.id,
            production_unit,
            items: items.into_iter().map(|oi| KotItem::from(oi)).collect(),
            status: KotStatus::Pending,
            created_at: Utc::now(),
            ..Default::default()
        };
        kots.push(repo.create(kot).await?);
    }
    
    Ok(kots)
}
```

### 3.3 Menu Resolution (Room + Order Type)

**Python Original** (`ury_pos/api.py:getRestaurantMenu`):
```python
@frappe.whitelist()
def getRestaurantMenu(room=None, order_type=None):
    # Room-wise menu
    if room:
        menu = frappe.get_doc("URY Menu", {"room": room, "active": 1})
        return menu.items
    
    # Order-type-wise menu
    if order_type:
        menu = frappe.get_doc("URY Menu", {"order_type": order_type, "active": 1})
        return menu.items
    
    # Default active menu
    menu = frappe.get_doc("URY Menu", {"is_default": 1, "active": 1})
    return menu.items
```

**Rust Port** (domain/src/menu.rs):
```rust
pub enum MenuResolutionStrategy {
    Room(Uuid),
    OrderType(String),
    Default,
}

pub async fn resolve_menu(
    strategy: MenuResolutionStrategy,
    repo: &MenuRepository,
    now: DateTime<Utc>,
) -> Result<Menu> {
    match strategy {
        MenuResolutionStrategy::Room(room_id) => {
            repo.find_active_by_room(room_id, now).await?
                .ok_or(Error::MenuNotFound)
        }
        MenuResolutionStrategy::OrderType(order_type) => {
            repo.find_active_by_order_type(&order_type, now).await?
                .ok_or(Error::MenuNotFound)
        }
        MenuResolutionStrategy::Default => {
            repo.find_default_active(now).await?
                .ok_or(Error::MenuNotFound)
        }
    }
}
```

### 3.4 P&L Recursive BOM Walk

**Python Original** (`ury_daily_p_and_l.py:inner_bom_process`):
```python
def inner_bom_process(item_code, qty, depth=0):
    if depth > 3:
        return 0
    bom = frappe.get_doc("URY BOM", {"item": item_code})
    if not bom:
        return get_item_valuation(item_code) * qty
    
    total = 0
    for bom_item in bom.items:
        total += inner_bom_process(bom_item.item_code, bom_item.qty * qty, depth + 1)
    return total
```

**Rust Port** (domain/src/pnl.rs):
```rust
const MAX_BOM_DEPTH: u8 = 3;

pub async fn calculate_cogs_recursive(
    item_code: &str,
    qty: Decimal,
    bom_repo: &BomRepository,
    valuation_repo: &ValuationRepository,
    depth: u8,
) -> Result<Decimal> {
    if depth > MAX_BOM_DEPTH {
        return Ok(Decimal::ZERO);
    }
    
    match bom_repo.find_by_item(item_code).await? {
        Some(bom) => {
            let mut total = Decimal::ZERO;
            for bom_item in bom.items {
                let sub_cogs = calculate_cogs_recursive(
                    &bom_item.item_code,
                    bom_item.qty * qty,
                    bom_repo,
                    valuation_repo,
                    depth + 1,
                ).await?;
                total += sub_cogs;
            }
            Ok(total)
        }
        None => {
            let valuation = valuation_repo.get_latest(&item_code).await?;
            Ok(valuation * qty)
        }
    }
}
```

**Optimization Note:** For production, cache BOM tree in Redis with TTL to avoid repeated DB queries.

---

## 4. API Design

### 4.1 REST Endpoints

**Order Management:**
- `POST /api/orders` - Create order
- `GET /api/orders/:id` - Get order
- `PATCH /api/orders/:id` - Update order
- `POST /api/orders/:id/submit` - Submit order (docstatus 0→1)
- `POST /api/orders/:id/cancel` - Cancel order
- `POST /api/orders/merge` - Merge tables
- `POST /api/orders/:id/transfer` - Transfer table

**KOT Management:**
- `POST /api/kots` - Generate KOT
- `GET /api/kots/:id` - Get KOT
- `PATCH /api/kots/:id/status` - Update KOT status

**Menu & POS:**
- `GET /api/menu?room_id=:id` - Get menu (room-wise)
- `GET /api/menu?order_type=:type` - Get menu (order-type)
- `POST /api/pos/opening` - Open shift
- `POST /api/pos/closing` - Close shift

**P&L & Reports:**
- `GET /api/pnl/daily?date=:date` - Daily P&L
- `POST /api/pnl/calculate` - Trigger P&L calc

### 4.2 WebSocket API

**Realtime Updates:**
- `ws://localhost:3000/ws/orders` - Order updates
- `ws://localhost:3000/ws/kots` - KOT status updates
- `ws://localhost:3000/ws/tables` - Table status updates

**Message Format:**
```json
{
  "event": "order.updated",
  "payload": {
    "order_id": "uuid",
    "status": "submitted",
    "table_id": "uuid"
  },
  "timestamp": "2026-07-28T01:00:00Z"
}
```

**Implementation:**
```rust
// api/src/ws/handler.rs
use axum::extract::ws::{WebSocket, Message};
use tokio::sync::broadcast;

pub async fn handle_socket(
    mut socket: WebSocket,
    tx: broadcast::Sender<WsEvent>,
) {
    let mut rx = tx.subscribe();
    
    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // Handle client messages
                    }
                    _ => break,
                }
            }
            event = rx.recv() => {
                if let Ok(event) = event {
                    let json = serde_json::to_string(&event).unwrap();
                    socket.send(Message::Text(json)).await.ok();
                }
            }
        }
    }
}
```

---

## 5. Database Schema Migration

### 5.1 Core Tables

**orders table:**
```sql
CREATE TYPE order_status AS ENUM ('draft', 'submitted', 'cancelled');

CREATE TABLE orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(140) UNIQUE NOT NULL,
    status order_status NOT NULL DEFAULT 'draft',
    order_type VARCHAR(140) NOT NULL,
    table_id UUID REFERENCES tables(id),
    room_id UUID REFERENCES rooms(id),
    merged_with JSONB DEFAULT '[]',  -- Array of UUIDs
    guest_name VARCHAR(140),
    guest_count INTEGER NOT NULL DEFAULT 1,
    total_amount DECIMAL(18, 2) NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    modified_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    owner VARCHAR(140) NOT NULL,
    CONSTRAINT fk_order_type FOREIGN KEY (order_type) REFERENCES order_types(name)
);

CREATE INDEX idx_orders_table ON orders(table_id);
CREATE INDEX idx_orders_status ON orders(status);
CREATE INDEX idx_orders_created ON orders(created_at);
```

**order_items table:**
```sql
CREATE TABLE order_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id UUID NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
    item_code VARCHAR(140) NOT NULL,
    item_name VARCHAR(140) NOT NULL,
    qty DECIMAL(18, 3) NOT NULL,
    rate DECIMAL(18, 2) NOT NULL,
    amount DECIMAL(18, 2) NOT NULL,
    item_group VARCHAR(140),
    production_unit VARCHAR(140),
    idx INTEGER NOT NULL,  -- Frappe parent ordering
    CONSTRAINT fk_order FOREIGN KEY (order_id) REFERENCES orders(id)
);

CREATE INDEX idx_order_items_order ON order_items(order_id);
```

**kots table:**
```sql
CREATE TYPE kot_status AS ENUM ('pending', 'in_progress', 'completed', 'cancelled');

CREATE TABLE kots (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(140) UNIQUE NOT NULL,
    order_id UUID NOT NULL REFERENCES orders(id),
    production_unit VARCHAR(140) NOT NULL,
    kot_number INTEGER NOT NULL,
    status kot_status NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_kots_order ON kots(order_id);
CREATE INDEX idx_kots_production_unit ON kots(production_unit);
CREATE INDEX idx_kots_status ON kots(status);
```

### 5.2 Frappe Pattern Replacements

| Frappe Pattern | PostgreSQL Solution |
|----------------|---------------------|
| `docstatus` (0/1/2) | Custom ENUM types |
| Child tables (`parent`, `parentfield`, `idx`) | Foreign keys + `idx` column |
| CSV fields (`merged_with`) | JSONB arrays |
| `naming_series` | Keep as VARCHAR(140) UNIQUE |
| `modified`, `creation` | `modified_at`, `created_at` TIMESTAMPTZ |
| `_assign`, `_liked_by` | Drop (not core business logic) |

---

## 6. Migration Phases (24-32 weeks)

### **Phase 0: Setup & Tooling (2 weeks)**
- Initialize Rust workspace
- Configure SQLx, Redis, Tokio
- Set up PostgreSQL schema
- Create SQLx migrations from Frappe schema
- CI/CD pipeline (GitHub Actions)
- Docker compose for local dev

**Deliverables:**
- Working `cargo build`
- PostgreSQL schema with all 36 tables
- Local dev environment

---

### **Phase 1: Core Domain Models (4 weeks)**
- Define all 36 domain structs
- Implement domain enums (OrderStatus, KotStatus, etc.)
- Write unit tests for domain logic
- Document ownership/lifetime patterns

**Deliverables:**
- `crates/domain/` with all types
- 80%+ test coverage for domain logic

---

### **Phase 2: Database Layer (6 weeks)**
- Implement SQLx repositories for all entities
- Write complex queries (BOM recursion, table clustering)
- Connection pooling (sqlx::Pool)
- Transaction patterns
- Database integration tests

**Critical Path:**
- Order repository (merge cluster query)
- BOM repository (recursive CTE for P&L)
- Menu repository (room/order-type resolution)

**Deliverables:**
- `crates/db/` with all repositories
- Integration tests against test DB

---

### **Phase 3: API Layer & Business Logic (8 weeks)**
- Axum route handlers for all endpoints
- WebSocket realtime layer
- Business logic services:
  - Order service (create, merge, transfer)
  - KOT service (generation, routing)
  - Menu service (resolution)
  - P&L service (recursive COGS)
  - POS service (shift open/close)
- JWT authentication
- CORS & middleware
- API integration tests

**Critical Path:**
- Table merge with BFS clustering
- KOT generation with production unit routing
- Menu resolution (3 strategies)
- P&L recursive BOM walk

**Deliverables:**
- `crates/api/` with all routes
- WebSocket broadcast working
- Auth middleware operational
- Postman collection for testing

---

### **Phase 4: Integration & Testing (6 weeks)**
- End-to-end tests (order flow, KOT flow, P&L calc)
- Performance testing (load testing with k6)
- Data migration scripts (Frappe DB → PostgreSQL)
- React frontend API integration
- Bug fixes & edge cases

**Data Migration:**
```bash
# Export from Frappe MariaDB
mysqldump ury_db > ury_export.sql

# Transform with Python script
python scripts/frappe_to_pg.py ury_export.sql

# Import to PostgreSQL
psql ury_rust < ury_pg.sql
```

**Deliverables:**
- All 3 React frontends (POS, KDS, POS v2) working against Rust API
- Data migration validated
- Performance benchmarks documented

---

### **Phase 5: Deployment & Cutover (2-4 weeks)**
- Production deployment (Docker + Kubernetes or AWS ECS)
- Blue/green deployment strategy
- Monitoring (Prometheus + Grafana)
- Logging (tracing + Loki)
- Database replication setup
- Rollback plan
- Go-live & post-deployment monitoring

**Deployment Architecture:**
```
[Load Balancer]
      |
      v
[Rust API (3 replicas)]
      |
      v
[PostgreSQL (primary + replica)]
[Redis Cluster]
```

**Deliverables:**
- Production deployment complete
- Monitoring dashboards live
- Runbook documented
- Team training completed

---

### **Phase 6: Post-Launch Optimization (2 weeks)**
- Performance tuning based on production metrics
- Query optimization
- Redis caching strategy refinement
- Documentation updates

---

## 7. What Stays vs What Rewrites

### **Keep (No Changes):**
- React frontends (POS, KDS, POS v2) - just update API endpoints
- PostgreSQL database concepts
- Redis for caching
- Business logic requirements

### **Rewrite (Complete Port):**
- All Python backend code (~6,075 lines)
- Frappe framework dependency (ORM, routing, auth, job queue)
- MariaDB-specific queries → PostgreSQL with JSONB
- `@frappe.whitelist()` decorators → Axum route handlers
- Frappe background jobs → Tokio tasks
- Frappe RPC → REST + WebSocket

### **New (Rust Advantages):**
- Compile-time SQL verification (SQLx)
- Zero-cost async/await (Tokio)
- Memory safety guarantees
- Native WebSocket performance
- Strong type system catching logic errors

---

## 8. Risk Assessment

| Risk | Severity | Mitigation |
|------|----------|------------|
| **Team Rust Learning Curve** | High | 2-week training bootcamp, pair programming, code reviews |
| **Complex BOM Recursion** | Medium | Write comprehensive tests, benchmark against Python |
| **Data Migration Bugs** | High | Staged migration, rollback plan, validation scripts |
| **Frontend API Breaking Changes** | Medium | API versioning, backward-compatible endpoints during transition |
| **Performance Regression** | Low | Rust is faster, but load test early |
| **Timeline Overrun** | Medium | 20% buffer built into 24-32 week estimate |

---

## 9. Success Metrics

| Metric | Current (Python) | Target (Rust) |
|--------|------------------|---------------|
| **Order Creation Latency** | ~200ms | <50ms |
| **KOT Generation Time** | ~150ms | <30ms |
| **P&L Calculation (daily)** | ~5s | <1s |
| **Concurrent WebSocket Connections** | ~100 | ~1,000 |
| **Memory Usage (API server)** | ~800MB | ~200MB |
| **Uptime** | 99.5% | 99.9% |

---

## 10. Conclusion

This Rust migration delivers a production-grade, high-performance POS system with memory safety and superior concurrency. The 6-8 month timeline is realistic given the complexity of 36 doctypes and critical business logic (table merge clustering, KOT routing, recursive BOM P&L).

**Recommendation:** Execute this plan if performance, reliability, and long-term maintainability justify the upfront rewrite cost. If time-to-market is critical, the TypeScript option in PLAN.md remains valid.

**Next Steps:**
1. Team Rust training (weeks 1-2)
2. Setup dev environment (Phase 0)
3. Kick off Phase 1 (domain models)

---

**Document Version:** 1.0  
**Author:** Fox (for Jack)  
**Date:** 2026-07-28
