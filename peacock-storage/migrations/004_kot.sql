-- Lane 2E: KOT (Kitchen Order Ticket) tables.
--
-- ## Gapless numbering
--
-- CGST Rule 46(b) demands gapless invoice numbering; KOTs are not tax documents but
-- the POS needs the same guarantee for audit visibility (every ticket printed must be
-- in the database, no skipped numbers). A PostgreSQL sequence guarantees this under
-- concurrent inserts when combined with SERIALIZABLE isolation and retry logic
-- (Storage::with_serializable_retry).
--
-- ## Production unit routing
--
-- `kots.production` is the station this ticket routes to. Query:
-- "pending KOTs for this production unit" drives the kitchen display.
--
-- ## N+1 query fix (bugs 6 and 7)
--
-- Upstream issued 36 queries for 12 items × 3 stations (ury_kot_generate.py:154, :214).
-- The fix: one query fetches all items for a set of KOTs, grouped by production unit.
-- `kot_items.idx` preserves the item order the domain assigned.

-- ---------------------------------------------------------------------------
-- kot_number_seq — gapless sequence per naming series
-- ---------------------------------------------------------------------------
-- One sequence per naming_series (e.g., "KOT-", "CNCL-"). The repository reads
-- `naming_series` from the Kot, then calls nextval on the matching sequence.
-- Creating sequences at INSERT time would race; they are created at migration time
-- or by an explicit "register naming series" call.

CREATE SEQUENCE kot_number_seq START WITH 1;

-- ---------------------------------------------------------------------------
-- KOT type enum
-- ---------------------------------------------------------------------------
-- Mirrors peacock_core::model::KotType exactly.

CREATE TYPE kot_type AS ENUM ('NewOrder', 'OrderModified', 'Cancelled', 'PartiallyCancelled');

-- ---------------------------------------------------------------------------
-- kots  (URY KOT root doctype)
-- ---------------------------------------------------------------------------
-- `name` is the Frappe autoname result: `naming_series` + sequence number + optional
-- suffix. Upstream's naming_series is a `Select` with `"KOT-\nCNCL-"`, so two prefixes
-- exist. The repository computes the name by reading the sequence and formatting it.
--
-- `invoice` is `Data` upstream, not a Link — it stores the raw POS Invoice name and
-- does NOT enforce an FK (ury_kot.json:71, "fieldtype": "Data").
--
-- `original_kot` is a CSV of the KOTs being cancelled (Small Text, :97). Kept as TEXT
-- to match the domain's `Option<String>` — parsing it is the cancel path's job.

CREATE TABLE kots (
    name                TEXT PRIMARY KEY,
    naming_series       TEXT        NOT NULL,
    invoice             TEXT        NOT NULL,
    restaurant_table    TEXT,
    customer_name       TEXT,
    original_kot        TEXT,
    date                DATE        NOT NULL,
    time                TIME,
    kot_type            kot_type    NOT NULL,
    order_status        TEXT,
    production          TEXT        REFERENCES production_units (name) ON UPDATE CASCADE ON DELETE RESTRICT,
    start_time_prep     TIME,
    pos_profile         TEXT,
    branch              TEXT,
    verified            BOOLEAN     NOT NULL DEFAULT FALSE,
    verified_by         TEXT,
    table_takeaway      BOOLEAN     NOT NULL DEFAULT FALSE,
    is_aggregator       BOOLEAN     NOT NULL DEFAULT FALSE,
    aggregator_id       TEXT,
    comments            TEXT,
    order_no            TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT kots_name_not_blank CHECK (length(btrim(name)) > 0),
    CONSTRAINT kots_naming_series_not_blank CHECK (length(btrim(naming_series)) > 0),
    CONSTRAINT kots_invoice_not_blank CHECK (length(btrim(invoice)) > 0)
);

-- KotRepo::exists_for probes (invoice, production) — the flip from NewOrder to
-- OrderModified (kot.rs:326). This index answers it without a table scan.
CREATE INDEX kots_exists_for_idx ON kots (invoice, production);

-- "Pending KOTs for this production unit" — the kitchen display query.
CREATE INDEX kots_production_idx ON kots (production, date, created_at);

-- Historical lookup: "all KOTs for this invoice".
CREATE INDEX kots_invoice_idx ON kots (invoice);

-- Branch-scoped reports.
CREATE INDEX kots_branch_idx ON kots (branch, date);

CREATE TRIGGER kots_set_updated_at
    BEFORE UPDATE ON kots
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ---------------------------------------------------------------------------
-- kot_items  (ury_kot_items child table)
-- ---------------------------------------------------------------------------
-- `idx` preserves the order the domain assigned. The parent KOT is deleted when the
-- parent is deleted (CASCADE), matching Frappe's child-table semantics.
--
-- `quantity` and `cancelled_qty` are `Data` (text) upstream (ury_kot.json:114, :119),
-- not numeric. Migration from Frappe must validate and report unparseable rows rather
-- than coercing to zero. Here they are NUMERIC because the Rust domain models them as
-- Decimal and every arithmetic operation expects that.

CREATE TABLE kot_items (
    id              BIGSERIAL PRIMARY KEY,
    kot_name        TEXT           NOT NULL
        REFERENCES kots (name) ON UPDATE CASCADE ON DELETE CASCADE,
    idx             INTEGER        NOT NULL,
    item            TEXT           NOT NULL
        REFERENCES items (code) ON UPDATE CASCADE ON DELETE RESTRICT,
    item_name       TEXT           NOT NULL,
    quantity        NUMERIC(18, 6) NOT NULL,
    cancelled_qty   NUMERIC(18, 6) NOT NULL DEFAULT 0,
    comments        TEXT,
    course          TEXT,
    serve_priority  INTEGER        NOT NULL DEFAULT 0,
    indicate_course BOOLEAN        NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ    NOT NULL DEFAULT now(),
    CONSTRAINT kot_items_idx_positive CHECK (idx > 0),
    CONSTRAINT kot_items_quantity_non_negative CHECK (quantity >= 0 AND cancelled_qty >= 0)
);

-- The N+1 fix: fetch all items for a set of KOTs in one query. The repository passes
-- `WHERE kot_name = ANY($1)` and the index covers it.
CREATE INDEX kot_items_kot_name_idx ON kot_items (kot_name, idx);

-- Item-level reporting: "which KOTs ordered this item on this date".
CREATE INDEX kot_items_item_idx ON kot_items (item);

-- Ordering within a KOT.
CREATE UNIQUE INDEX kot_items_order_idx ON kot_items (kot_name, idx);
