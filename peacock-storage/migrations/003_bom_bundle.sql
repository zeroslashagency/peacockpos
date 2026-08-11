-- Lane 2D: BOM and Product Bundle tables.
--
-- These four tables are the storage behind `peacock_core::ports::BomRepo` and
-- `ProductBundleRepo`, which together feed every one of the three cost bases in
-- `peacock_core::cogs` (bundle -> BOM -> plain item).
--
-- ---------------------------------------------------------------------------
-- The v1 10x bug, and why it cannot happen through this schema
-- ---------------------------------------------------------------------------
-- Upstream normalises a batch BOM to a per-unit cost by dividing the summed line
-- cost by the BOM's own batch size (ury_daily_p_and_l.py:38, :57):
--
--     bom_buying_price = bom_buying_price / bom.quantity
--
-- v1 dropped that division, so a BOM whose batch produces 10 units priced every
-- unit at the whole batch cost: a 70 rupee batch of 10 cups of Masala Chai became
-- 70 rupees per cup instead of 7. Three things here make the same mistake
-- impossible to reintroduce:
--
--   1. `boms.quantity` is NOT NULL. There is no default and no nullable path, so a
--      row cannot exist without the divisor. A repository that forgets to select it
--      fails to build a `Bom`, rather than quietly defaulting to 1.
--   2. `boms.quantity` is CHECK (quantity > 0). `cogs::bom_cost_per_unit` still
--      raises `Error::BomZeroQuantity` for non-Postgres sources, but no row that
--      reaches that guard can come from here.
--   3. `bom_lines.quantity` is NOT NULL and carries its own 6-decimal NUMERIC, kept
--      distinct from the parent's batch quantity by name and by table. The two
--      quantities are the two halves of the normalisation and are never conflated.
--
-- Both quantities are NUMERIC(18,6), matching `item_prices.rate`: a per-unit
-- ingredient cost is divided by the batch size before it is ever rounded to paisa,
-- so truncating either input earlier would move COGS.
--
-- ---------------------------------------------------------------------------
-- Two levels, and why the schema does not enforce that
-- ---------------------------------------------------------------------------
-- `cogs::MAX_LEVEL` is 2: `inner_bom_process` (:10) descends once into
-- `inner_inner_bom_process` (:42), which prices every line from `Item Price` and
-- stops. A third-level BOM is therefore treated as a leaf, priced directly, and NOT
-- exploded.
--
-- That is a property of the walk, not of the data. This schema deliberately allows a
-- BOM graph of any depth (a BOM whose line has a BOM whose line has a BOM) because
-- upstream allows it and the third level existing is exactly what fixture
-- `09_cogs_three_level_max_depth.json` asserts gets ignored. Capping depth in SQL
-- would make that fixture unrepresentable and would diverge from upstream.
--
-- Self-reference is the one exception: a BOM line pointing at its own parent's item
-- is rejected, because it is never legitimate and the walk's depth limit would hide
-- it as a silently truncated cost rather than surfacing it.
--
-- ---------------------------------------------------------------------------
-- Bundle beats BOM, and neither is a fallback for the other
-- ---------------------------------------------------------------------------
-- Upstream partitions invoice lines with three mutually exclusive queries: the
-- bundle bucket is `d.new_item_code IS NOT NULL` (:170) and does not join `tabBOM`
-- at all, while the plain and BOM buckets both require `d.new_item_code IS NULL`
-- (:102, :139). An item that is both a Product Bundle and has an active default BOM
-- is priced as a bundle, and its own BOM is never consulted.
--
-- The precedence lives in `cogs::cogs_for_item_with_bundles`, which asks
-- `ProductBundleRepo` first. What this schema contributes is that neither lookup can
-- be ambiguous:
--
--   * `product_bundles.new_item_code` is UNIQUE, so "the bundle sold under this item
--     code" is one row or none.
--   * `boms` has a partial unique index over (item) restricted to the active,
--     default, submitted, not-deleted rows, so "the BOM for this item" is likewise
--     one row or none.
--
-- Upstream took `boms[0]` and `pb_items[0]` from an unordered result set, so with
-- duplicates its answer depended on physical row order. Ours cannot.
--
-- ---------------------------------------------------------------------------
-- Filters: the BOM lookup has them, the bundle lookup does not
-- ---------------------------------------------------------------------------
-- BOM (ury_daily_p_and_l.py:19, :227): `is_active=1 AND is_default=1 AND docstatus=1`.
-- Product Bundle (:222): no `docstatus` filter, no `is_active` filter. A draft bundle
-- still captures the item, which `ports.rs` calls out explicitly. So there is no
-- status column on `product_bundles` at all -- not an unused one that a later change
-- might start filtering on.
--
-- `docstatus` becomes `bom_status` here, per the plan's "docstatus -> status ENUM per
-- entity" rule. Only 'Submitted' is priced; 'Draft' and 'Cancelled' are invisible to
-- `BomRepo::find_for_item`.

-- ---------------------------------------------------------------------------
-- BOM status enum (Frappe docstatus 0/1/2)
-- ---------------------------------------------------------------------------

CREATE TYPE bom_status AS ENUM ('Draft', 'Submitted', 'Cancelled');

-- ---------------------------------------------------------------------------
-- boms  (ERPNext BOM)
-- ---------------------------------------------------------------------------
-- `name` is the Frappe docname (BOM-ITEM-001). `peacock_core::ids::BomName` is a
-- newtype over String and `Error::BomZeroQuantity` reports it, so it stays TEXT.

CREATE TABLE boms (
    name       TEXT           PRIMARY KEY,
    item       TEXT           NOT NULL
        REFERENCES items (code) ON UPDATE CASCADE ON DELETE CASCADE,
    -- The divisor. See the header: NOT NULL and strictly positive is what makes v1's
    -- missing normalisation unrepresentable rather than merely discouraged.
    quantity   NUMERIC(18, 6) NOT NULL,
    uom        TEXT,
    is_active  BOOLEAN        NOT NULL DEFAULT TRUE,
    is_default BOOLEAN        NOT NULL DEFAULT TRUE,
    status     bom_status     NOT NULL DEFAULT 'Draft',
    created_at TIMESTAMPTZ    NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ    NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ,
    CONSTRAINT boms_name_not_blank CHECK (length(btrim(name)) > 0),
    CONSTRAINT boms_quantity_positive CHECK (quantity > 0)
);

-- BomRepo::find_for_item is the only read path, and it is called once per BOM line
-- during the explosion, so it has to be an index hit.
CREATE INDEX boms_item_idx ON boms (item) WHERE deleted_at IS NULL;

-- Exactly the predicate of that lookup. Upstream's `boms[0]` picked arbitrarily among
-- duplicates; this makes duplicates impossible, so COGS cannot depend on row order.
CREATE UNIQUE INDEX boms_one_active_default_per_item
    ON boms (item)
    WHERE is_active AND is_default AND status = 'Submitted' AND deleted_at IS NULL;

CREATE TRIGGER boms_set_updated_at
    BEFORE UPDATE ON boms
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ---------------------------------------------------------------------------
-- bom_lines  (BOM Item child table)
-- ---------------------------------------------------------------------------
-- `idx` is the Frappe child-table ordering column and part of the primary key, so
-- `Bom::items` reads back in the order the operator entered it.
--
-- The primary key is (bom, idx), NOT (bom, item_code): ERPNext permits the same
-- ingredient on two lines, and upstream costs each line independently. Keying on
-- item_code would collapse two 5g lines into one and halve that ingredient's cost.
--
-- `quantity` is the per-batch consumption of this ingredient. Zero is legal (an
-- optional garnish costed at nothing, covered by `cogs::bom_line_with_zero_qty`);
-- negative is not, since it would subtract from the batch cost.

CREATE TABLE bom_lines (
    bom        TEXT           NOT NULL
        REFERENCES boms (name) ON UPDATE CASCADE ON DELETE CASCADE,
    idx        INTEGER        NOT NULL,
    item_code  TEXT           NOT NULL
        REFERENCES items (code) ON UPDATE CASCADE ON DELETE RESTRICT,
    quantity   NUMERIC(18, 6) NOT NULL,
    uom        TEXT,
    created_at TIMESTAMPTZ    NOT NULL DEFAULT now(),
    PRIMARY KEY (bom, idx),
    CONSTRAINT bom_lines_idx_positive CHECK (idx > 0),
    CONSTRAINT bom_lines_quantity_non_negative CHECK (quantity >= 0)
);

-- "Which BOMs consume this ingredient" — the direction a price change walks.
CREATE INDEX bom_lines_item_code_idx ON bom_lines (item_code);

-- A line whose item is its own BOM's product would be an infinite explosion that
-- MAX_LEVEL would silently truncate into an understated cost instead of an error.
CREATE OR REPLACE FUNCTION bom_lines_reject_self_reference() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    parent_item TEXT;
BEGIN
    SELECT item INTO parent_item FROM boms WHERE name = NEW.bom;
    IF parent_item = NEW.item_code THEN
        RAISE EXCEPTION
            'BOM % cannot consume its own output item %', NEW.bom, NEW.item_code
            USING ERRCODE = 'check_violation',
                  CONSTRAINT = 'bom_lines_no_self_reference';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER bom_lines_no_self_reference
    BEFORE INSERT OR UPDATE ON bom_lines
    FOR EACH ROW EXECUTE FUNCTION bom_lines_reject_self_reference();

-- ---------------------------------------------------------------------------
-- items.is_bom — kept honest by trigger
-- ---------------------------------------------------------------------------
-- 001_core_tables.sql describes `items.is_bom` as a cache of "a default active BOM
-- exists" and leaves maintaining it to this lane. Recomputed from the same predicate
-- `BomRepo::find_for_item` uses, so the flag and the lookup can never disagree.
--
-- The write is guarded on an actual change: `items` has an updated_at trigger, and
-- touching every item on every BOM edit would move timestamps for nothing.

CREATE OR REPLACE FUNCTION refresh_item_is_bom(p_item TEXT) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    has_bom BOOLEAN;
BEGIN
    IF p_item IS NULL THEN
        RETURN;
    END IF;

    SELECT EXISTS (
        SELECT 1 FROM boms
        WHERE item = p_item
          AND is_active
          AND is_default
          AND status = 'Submitted'
          AND deleted_at IS NULL
    ) INTO has_bom;

    UPDATE items
       SET is_bom = has_bom
     WHERE code = p_item
       AND is_bom IS DISTINCT FROM has_bom;
END;
$$;

CREATE OR REPLACE FUNCTION boms_sync_item_is_bom() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP <> 'INSERT' THEN
        PERFORM refresh_item_is_bom(OLD.item);
    END IF;
    IF TG_OP <> 'DELETE' THEN
        PERFORM refresh_item_is_bom(NEW.item);
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER boms_sync_item_is_bom
    AFTER INSERT OR UPDATE OR DELETE ON boms
    FOR EACH ROW EXECUTE FUNCTION boms_sync_item_is_bom();

-- ---------------------------------------------------------------------------
-- product_bundles  (ERPNext Product Bundle)
-- ---------------------------------------------------------------------------
-- `new_item_code` is the code that appears on the POS Invoice line and the key
-- `ProductBundleRepo::find_by_new_item_code` looks up (:82, :222). UNIQUE because the
-- lookup must resolve to one bundle: two bundles selling the same code would make the
-- cost basis depend on row order.
--
-- No status column, on purpose — see the header. Upstream applies no docstatus or
-- is_active filter to Product Bundle, so a draft bundle still captures the item, and
-- an unused status column here would be an invitation to break that.
--
-- The Frappe docname is kept as the primary key even though `ProductBundle` does not
-- model it: child rows need a parent to point at, and upstream reads it (:222-223).

CREATE TABLE product_bundles (
    name           TEXT        PRIMARY KEY,
    new_item_code  TEXT        NOT NULL
        REFERENCES items (code) ON UPDATE CASCADE ON DELETE CASCADE,
    description    TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at     TIMESTAMPTZ,
    CONSTRAINT product_bundles_name_not_blank CHECK (length(btrim(name)) > 0)
);

-- Partial rather than a plain UNIQUE constraint: a retired bundle must not block a
-- replacement from being created for the same item code.
CREATE UNIQUE INDEX product_bundles_new_item_code_idx
    ON product_bundles (new_item_code) WHERE deleted_at IS NULL;

CREATE TRIGGER product_bundles_set_updated_at
    BEFORE UPDATE ON product_bundles
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ---------------------------------------------------------------------------
-- product_bundle_lines  (Product Bundle Item child table)
-- ---------------------------------------------------------------------------
-- Same child-table shape as bom_lines and for the same reasons: (bundle, idx) as the
-- key so duplicate component lines are both costed, `idx` so `ProductBundle::items`
-- reads back in entry order.
--
-- A bundle line is priced by asking `BomRepo` for its item and otherwise falling back
-- to `Item Price` (:227-243), so a line's item may or may not have a BOM and may
-- itself be a bundle — upstream never re-queries Product Bundle here, and prices such
-- a child as a leaf (`cogs::bundle_of_bundle_inner_priced_as_leaf`). Nothing in this
-- table needs to know which case applies.
--
-- Self-reference is rejected for the same reason as bom_lines: a bundle containing
-- its own sold item is not a cost, it is a data entry error.

CREATE TABLE product_bundle_lines (
    bundle      TEXT           NOT NULL
        REFERENCES product_bundles (name) ON UPDATE CASCADE ON DELETE CASCADE,
    idx         INTEGER        NOT NULL,
    item_code   TEXT           NOT NULL
        REFERENCES items (code) ON UPDATE CASCADE ON DELETE RESTRICT,
    quantity    NUMERIC(18, 6) NOT NULL,
    uom         TEXT,
    description TEXT,
    created_at  TIMESTAMPTZ    NOT NULL DEFAULT now(),
    PRIMARY KEY (bundle, idx),
    CONSTRAINT product_bundle_lines_idx_positive CHECK (idx > 0),
    CONSTRAINT product_bundle_lines_quantity_non_negative CHECK (quantity >= 0)
);

CREATE INDEX product_bundle_lines_item_code_idx ON product_bundle_lines (item_code);

CREATE OR REPLACE FUNCTION product_bundle_lines_reject_self_reference() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    sold_item TEXT;
BEGIN
    SELECT new_item_code INTO sold_item FROM product_bundles WHERE name = NEW.bundle;
    IF sold_item = NEW.item_code THEN
        RAISE EXCEPTION
            'product bundle % cannot contain its own sold item %', NEW.bundle, NEW.item_code
            USING ERRCODE = 'check_violation',
                  CONSTRAINT = 'product_bundle_lines_no_self_reference';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER product_bundle_lines_no_self_reference
    BEFORE INSERT OR UPDATE ON product_bundle_lines
    FOR EACH ROW EXECUTE FUNCTION product_bundle_lines_reject_self_reference();
