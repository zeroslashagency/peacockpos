-- Lane 2A: core tables.
--
-- Frappe → PostgreSQL mapping rules applied here (PHASE_2_3_PLAN.md §"Schema Design Principles"):
--   * Frappe docnames are user-visible TEXT (autoname `prompt` / `field:`), and the
--     domain models them as newtypes over String (peacock-core/src/ids.rs). So every
--     primary key here is TEXT, never a surrogate integer.
--   * `merged_with` is CSV in a Frappe `Data` field. Here it is a JSONB array — the CSV
--     parser (`MergedWith::parse`) stays for reading legacy rows only.
--   * `docstatus` is deliberately absent. Each entity gets its own status enum in the
--     lane that owns it (2E KOT, 2F invoice, 2G shift).
--   * `created_at` / `updated_at` TIMESTAMPTZ on every table, `updated_at` maintained by
--     trigger so repositories cannot forget it.
--   * `deleted_at` soft-delete column on the entities the POS UI can retire without
--     breaking historical invoices (restaurants, tables, production units, items).
--     Price rows are hard-deleted: they carry no history worth keeping.

-- ---------------------------------------------------------------------------
-- updated_at trigger
-- ---------------------------------------------------------------------------

CREATE OR REPLACE FUNCTION set_updated_at() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    NEW.updated_at := now();
    RETURN NEW;
END;
$$;

-- ---------------------------------------------------------------------------
-- jsonb_is_text_array — guard for JSONB columns that must hold Vec<String>
-- ---------------------------------------------------------------------------
-- A CHECK constraint cannot contain a subquery or a set-returning function, and
-- `jsonb_array_elements` is both. Wrapping the test in an IMMUTABLE function is the
-- supported way to get the same guarantee: `tables.merged_with` deserialises into
-- `MergedWith(Vec<TableName>)`, so an object, a scalar or an array with a non-string
-- element has to be rejected at write time rather than panicking a repository at read
-- time.

CREATE OR REPLACE FUNCTION jsonb_is_text_array(v jsonb) RETURNS boolean
LANGUAGE sql IMMUTABLE PARALLEL SAFE STRICT AS $$
    SELECT jsonb_typeof(v) = 'array'
       AND NOT EXISTS (
               SELECT 1 FROM jsonb_array_elements(v) AS e
               WHERE jsonb_typeof(e) <> 'string'
           );
$$;

-- ---------------------------------------------------------------------------
-- restaurants  (URY Restaurant; `branch` field kept because Table.branch links it)
-- ---------------------------------------------------------------------------
-- Upstream fields (ury_restaurant.json): company, invoice_series_prefix, active_menu,
-- branch, default_tax_template, address, room_wise_menu, default_room,
-- aggregator_series_prefix, order_type_wise_menu.
--
-- `pos_profile` is NOT on the upstream doctype — POS Profile is reached from the
-- production unit / invoice side. It is carried here as a nullable convenience link
-- because the Lane 2A brief names it; nullable so it cannot become a false constraint.
-- Menu / order-type child tables (`menu_for_room`, `order_type_menu`) belong to
-- Lane 2C's 002_menu_tables.sql, and `active_menu` gets its FK there.

CREATE TABLE restaurants (
    name                     TEXT PRIMARY KEY,
    company                  TEXT        NOT NULL,
    branch                   TEXT        NOT NULL,
    pos_profile              TEXT,
    invoice_series_prefix    TEXT        NOT NULL,
    aggregator_series_prefix TEXT,
    active_menu              TEXT,
    default_room             TEXT,
    default_tax_template     TEXT,
    address                  TEXT,
    room_wise_menu           BOOLEAN     NOT NULL DEFAULT FALSE,
    order_type_wise_menu     BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at               TIMESTAMPTZ,
    CONSTRAINT restaurants_name_not_blank CHECK (length(btrim(name)) > 0),
    -- CGST Rule 46(b): the prefix becomes part of the invoice name, which is capped
    -- at 16 chars overall (peacock-core Error::InvoiceNameTooLong). A prefix longer
    -- than the cap can never produce a legal name.
    CONSTRAINT restaurants_invoice_prefix_len CHECK (length(invoice_series_prefix) BETWEEN 1 AND 16)
);

CREATE INDEX restaurants_branch_idx ON restaurants (branch) WHERE deleted_at IS NULL;
CREATE INDEX restaurants_pos_profile_idx ON restaurants (pos_profile) WHERE deleted_at IS NULL;

CREATE TRIGGER restaurants_set_updated_at
    BEFORE UPDATE ON restaurants
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ---------------------------------------------------------------------------
-- rooms  (URY Room) — required for tables.restaurant_room to be a real FK
-- ---------------------------------------------------------------------------
-- `TableRepo::list_by_room` is room-scoped and `merge.rs` refuses cross-room merges
-- (Error::CrossRoomMerge), so the room reference has to be enforceable, not advisory.

CREATE TABLE rooms (
    name       TEXT PRIMARY KEY,
    branch     TEXT        NOT NULL,
    room_type  TEXT        CHECK (room_type IN ('AC', 'NON-AC')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ,
    CONSTRAINT rooms_name_not_blank CHECK (length(btrim(name)) > 0)
);

CREATE INDEX rooms_branch_idx ON rooms (branch) WHERE deleted_at IS NULL;

CREATE TRIGGER rooms_set_updated_at
    BEFORE UPDATE ON rooms
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

ALTER TABLE restaurants
    ADD CONSTRAINT restaurants_default_room_fkey
    FOREIGN KEY (default_room) REFERENCES rooms (name) ON UPDATE CASCADE ON DELETE SET NULL;

-- ---------------------------------------------------------------------------
-- tables  (URY Table)
-- ---------------------------------------------------------------------------
-- Mirrors peacock_core::model::Table one-to-one.
--   * layout_* are Float upstream and geometry, not money → double precision is correct.
--   * latest_invoice_time is a bare `Time` upstream → TIME WITHOUT TIME ZONE
--     (NaiveTime in the domain). Business-day date lives on the invoice, not here.
--   * merged_with: JSONB array of table names. The CHECK keeps it an array so a repo
--     cannot smuggle in an object or scalar and break `MergedWith` deserialisation.
--     Self-membership is rejected: merge.rs treats the seed implicitly.
-- "tables" is not reserved in Postgres, but it collides with information_schema.tables
-- in unqualified queries, so repos must qualify it as public.tables or use the alias.

CREATE TABLE tables (
    name                TEXT PRIMARY KEY,
    no_of_seats         INTEGER          NOT NULL DEFAULT 0,
    minimum_seating     INTEGER          NOT NULL DEFAULT 0,
    restaurant          TEXT             NOT NULL
        REFERENCES restaurants (name) ON UPDATE CASCADE ON DELETE RESTRICT,
    restaurant_room     TEXT             NOT NULL
        REFERENCES rooms (name) ON UPDATE CASCADE ON DELETE RESTRICT,
    branch              TEXT             NOT NULL,
    is_take_away        BOOLEAN          NOT NULL DEFAULT FALSE,
    occupied            BOOLEAN          NOT NULL DEFAULT FALSE,
    latest_invoice_time TIME,
    table_shape         TEXT             CHECK (table_shape IN ('Rectangle', 'Square', 'Circle')),
    layout_x            DOUBLE PRECISION NOT NULL DEFAULT 0,
    layout_y            DOUBLE PRECISION NOT NULL DEFAULT 0,
    layout_width        DOUBLE PRECISION NOT NULL DEFAULT 0,
    layout_height       DOUBLE PRECISION NOT NULL DEFAULT 0,
    merged_with         JSONB            NOT NULL DEFAULT '[]'::jsonb,
    created_at          TIMESTAMPTZ      NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ      NOT NULL DEFAULT now(),
    deleted_at          TIMESTAMPTZ,
    CONSTRAINT tables_name_not_blank CHECK (length(btrim(name)) > 0),
    CONSTRAINT tables_seats_non_negative CHECK (no_of_seats >= 0 AND minimum_seating >= 0),
    CONSTRAINT tables_merged_with_is_text_array CHECK (jsonb_is_text_array(merged_with)),
    CONSTRAINT tables_merged_with_excludes_self CHECK (NOT (merged_with ? name))
);

-- TableRepo::list_by_room does one query per room; the merge BFS must not re-query
-- per hop, so this index carries the whole room scan.
CREATE INDEX tables_restaurant_room_idx ON tables (restaurant, restaurant_room)
    WHERE deleted_at IS NULL;
CREATE INDEX tables_room_idx ON tables (restaurant_room) WHERE deleted_at IS NULL;
-- `status` on URY Table is the pair (occupied, is_take_away); there is no status
-- column upstream. Partial index on occupied covers the "free tables" screen.
CREATE INDEX tables_occupied_idx ON tables (restaurant_room, occupied) WHERE deleted_at IS NULL;
CREATE INDEX tables_branch_idx ON tables (branch) WHERE deleted_at IS NULL;
-- GIN so "which cluster is table X in" is an index hit, not a full scan of every
-- merged_with array.
CREATE INDEX tables_merged_with_gin ON tables USING gin (merged_with);

CREATE TRIGGER tables_set_updated_at
    BEFORE UPDATE ON tables
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ---------------------------------------------------------------------------
-- production_units  (URY Production Unit) + its item_groups child table
-- ---------------------------------------------------------------------------
-- `production` is the autoname field and is unique upstream, so it IS the name.
-- ProductionRepo::list_for_branch drives KOT station routing, hence the branch index.

CREATE TABLE production_units (
    name        TEXT PRIMARY KEY,
    branch      TEXT        NOT NULL,
    pos_profile TEXT,
    warehouse   TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at  TIMESTAMPTZ,
    CONSTRAINT production_units_name_not_blank CHECK (length(btrim(name)) > 0)
);

CREATE INDEX production_units_branch_idx ON production_units (branch) WHERE deleted_at IS NULL;
CREATE INDEX production_units_pos_profile_idx ON production_units (pos_profile) WHERE deleted_at IS NULL;

CREATE TRIGGER production_units_set_updated_at
    BEFORE UPDATE ON production_units
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ury_production_item_groups (istable=1) → FK + `idx` ordering column, per the plan's
-- child-table rule. ProductionUnit::item_groups is a Vec, so order must be stable.
CREATE TABLE production_unit_item_groups (
    production_unit TEXT        NOT NULL
        REFERENCES production_units (name) ON UPDATE CASCADE ON DELETE CASCADE,
    idx             INTEGER     NOT NULL,
    item_group      TEXT        NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (production_unit, item_group),
    CONSTRAINT production_unit_item_groups_idx_positive CHECK (idx > 0)
);

-- Routing asks "which units claim this item group" for a branch: index the group side.
CREATE INDEX production_unit_item_groups_group_idx
    ON production_unit_item_groups (item_group);
CREATE UNIQUE INDEX production_unit_item_groups_order_idx
    ON production_unit_item_groups (production_unit, idx);

-- ---------------------------------------------------------------------------
-- items  (ERPNext Item — URY only links to it)
-- ---------------------------------------------------------------------------
-- Only the columns Peacock actually reads:
--   * item_group  → KOT station routing (ItemRepo::item_groups, fixes bugs 6 & 7)
--   * is_bom      → whether a BOM lookup is worth attempting (cogs.rs)
--   * stock_uom   → needed to interpret BOM quantities
-- `is_bom` is a cache of "a default active BOM exists"; Lane 2D owns the BOM tables and
-- may add a trigger to keep it honest. Do not treat it as authoritative on its own.

CREATE TABLE items (
    code        TEXT PRIMARY KEY,
    name        TEXT        NOT NULL,
    item_group  TEXT,
    stock_uom   TEXT        NOT NULL DEFAULT 'Nos',
    is_bom      BOOLEAN     NOT NULL DEFAULT FALSE,
    disabled    BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at  TIMESTAMPTZ,
    CONSTRAINT items_code_not_blank CHECK (length(btrim(code)) > 0),
    CONSTRAINT items_name_not_blank CHECK (length(btrim(name)) > 0)
);

-- ItemRepo::item_groups batches `WHERE code = ANY($1)`; the PK covers that.
-- This one covers the reverse direction used by station routing.
CREATE INDEX items_item_group_idx ON items (item_group) WHERE deleted_at IS NULL;
CREATE INDEX items_is_bom_idx ON items (code) WHERE is_bom AND deleted_at IS NULL;

CREATE TRIGGER items_set_updated_at
    BEFORE UPDATE ON items
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ---------------------------------------------------------------------------
-- price_lists + item_prices  (ERPNext Price List / Item Price)
-- ---------------------------------------------------------------------------
-- COGS prices from the *buying* price list (ury_daily_p_and_l.py:30), never from stock
-- valuation, so buying/selling has to be distinguishable. `PriceRepo::item_price`
-- returns Option — a missing row is normal and surfaces as `unset_*_prices`, not an error.

CREATE TABLE price_lists (
    name       TEXT PRIMARY KEY,
    currency   TEXT        NOT NULL DEFAULT 'INR',
    buying     BOOLEAN     NOT NULL DEFAULT FALSE,
    selling    BOOLEAN     NOT NULL DEFAULT FALSE,
    enabled    BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT price_lists_name_not_blank CHECK (length(btrim(name)) > 0),
    CONSTRAINT price_lists_has_a_direction CHECK (buying OR selling)
);

CREATE TRIGGER price_lists_set_updated_at
    BEFORE UPDATE ON price_lists
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- rate is NUMERIC(18,6): money never touches float (peacock-core/src/money.rs).
-- 6 dp because a per-unit ingredient rate is divided by BOM batch size before it is
-- rounded to paisa, and truncating the input early would move COGS.
--
-- valid_from is nullable and part of the uniqueness key: ERPNext allows a dated price
-- history per (item, price_list). A NULL valid_from is the open-ended base rate, and
-- NULLS NOT DISTINCT is what stops two competing base rates from coexisting.
CREATE TABLE item_prices (
    id          BIGSERIAL PRIMARY KEY,
    item_code   TEXT           NOT NULL
        REFERENCES items (code) ON UPDATE CASCADE ON DELETE CASCADE,
    price_list  TEXT           NOT NULL
        REFERENCES price_lists (name) ON UPDATE CASCADE ON DELETE CASCADE,
    rate        NUMERIC(18, 6) NOT NULL,
    currency    TEXT           NOT NULL DEFAULT 'INR',
    uom         TEXT,
    valid_from  DATE,
    valid_upto  DATE,
    created_at  TIMESTAMPTZ    NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ    NOT NULL DEFAULT now(),
    CONSTRAINT item_prices_validity_ordered CHECK (
        valid_from IS NULL OR valid_upto IS NULL OR valid_from <= valid_upto
    )
);

CREATE UNIQUE INDEX item_prices_unique_idx
    ON item_prices (item_code, price_list, valid_from) NULLS NOT DISTINCT;
-- The exact shape of PriceRepo::item_price's lookup.
CREATE INDEX item_prices_lookup_idx ON item_prices (item_code, price_list);
CREATE INDEX item_prices_price_list_idx ON item_prices (price_list);

CREATE TRIGGER item_prices_set_updated_at
    BEFORE UPDATE ON item_prices
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
