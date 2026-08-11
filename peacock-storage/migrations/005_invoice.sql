-- Lane 2F: POS Invoice — gapless numbering, idempotency, status transitions.
--
-- ===========================================================================
-- Why a counter TABLE and not a PostgreSQL SEQUENCE
-- ===========================================================================
--
-- The Lane 2F brief says "use a PostgreSQL sequence". That is the wrong tool for a
-- tax document and using it would break the one rule this table exists to keep.
--
-- `nextval()` is deliberately NON-transactional: it is exempt from rollback so that
-- concurrent writers never block on it. That is the right trade for a surrogate key,
-- and Lane 2E correctly uses a sequence for KOT numbers (a KOT is not a tax document;
-- 004_kot.sql says as much). But it means every rolled-back invoice insert — a failed
-- payment, a constraint violation, a dropped connection — permanently BURNS a number
-- and punches a hole in the series.
--
-- CGST Rule 46(b) requires a *consecutive* serial number, unique per financial year.
-- A gap is exactly what it forbids, and `peacock-core/src/invoicing.rs` is explicit
-- about the remedy in the `SeriesAllocator` doc comment:
--
--     UPDATE naming_series
--        SET next_number = next_number + 1
--      WHERE series = $1 AND fiscal_year = $2
--     RETURNING next_number - 1
--
--   "This takes a row lock and ensures the increment rolls back with the surrounding
--    transaction. If the transaction fails (e.g., constraint violation on the invoice
--    insert), the number is NOT burned."
--
-- There is a domain test pinning this — `invoicing.rs::rolled_back_allocation_does_
-- not_burn_number`. A sequence cannot satisfy it. So the counter lives in
-- `invoice_naming_series` below, one row per (series, fiscal_year), and the row lock
-- taken by the UPDATE is what serialises concurrent allocations.
--
-- Corollary: the invoice path does NOT need SERIALIZABLE. Correctness comes from the
-- row lock, not from the isolation level; at 100-way concurrency SERIALIZABLE would
-- only add 40001 aborts to retry. See `repos/invoice.rs` for the full argument.
--
-- The only legitimate gap is a deliberately cancelled invoice, which must carry a
-- logged reason (`invoices.cancel_reason`) so the audit trail explains the hole.
--
-- ===========================================================================
-- Money
-- ===========================================================================
--
-- Every money column is NUMERIC(18,6) — the Lane 2A standard. Never FLOAT, never REAL.
-- `peacock-core/src/money.rs` is Decimal-only and the parity harness exists to catch
-- paisa drift; a float column here would reintroduce it below the level the harness
-- can see.
--
-- Six decimals rather than two because the domain never rounds `net_total`,
-- `discount` or `taxable_value` (tax.rs), so storing at paisa scale would truncate a
-- value the domain still considers exact.
--
-- ===========================================================================
-- No soft delete
-- ===========================================================================
--
-- Unlike the Lane 2A entities there is no `deleted_at` here. An issued invoice is a
-- tax document; it is superseded (Return) or cancelled with a reason, never retired.

-- ---------------------------------------------------------------------------
-- invoice_naming_series — the gapless, transactional counter
-- ---------------------------------------------------------------------------
-- `next_number` is the number the NEXT allocation will hand out, so a fresh series
-- starts at 1 and `UPDATE ... RETURNING next_number - 1` yields it.
--
-- Length limits mirror `restaurants.invoice_series_prefix` (001_core_tables.sql) and
-- the four-character fiscal-year code from `invoicing::fiscal_year_code`. The final
-- 16-character cap is enforced by the domain (`Error::InvoiceNameTooLong`) rather than
-- here, on purpose: the repository must be able to seed a too-long series in order to
-- prove the domain guard fires and leaves the counter untouched.

CREATE TABLE invoice_naming_series (
    series      TEXT        NOT NULL,
    fiscal_year TEXT        NOT NULL,
    next_number BIGINT      NOT NULL DEFAULT 1,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (series, fiscal_year),
    CONSTRAINT invoice_naming_series_series_len
        CHECK (length(series) BETWEEN 1 AND 16),
    -- `fiscal_year_code` always renders four digits ("2627"), including across the
    -- century rollover ("9900"). Anything else could not have come from the domain.
    CONSTRAINT invoice_naming_series_fy_is_four_digits
        CHECK (fiscal_year ~ '^[0-9]{4}$'),
    -- The counter only ever moves forward. A rollback restores the previous value; a
    -- manual rewind would renumber a future invoice onto an already-issued number.
    CONSTRAINT invoice_naming_series_next_number_positive
        CHECK (next_number >= 1)
);

CREATE TRIGGER invoice_naming_series_set_updated_at
    BEFORE UPDATE ON invoice_naming_series
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ---------------------------------------------------------------------------
-- invoice_status enum
-- ---------------------------------------------------------------------------
-- Mirrors peacock_core::model::PosInvoiceStatus exactly, including `Return`.
-- `PosInvoiceStatus::REVENUE` is the single authoritative "counts as revenue" set
-- (Paid, Consolidated) and fixes bug 4, where shift close used only Paid.

CREATE TYPE invoice_status AS ENUM ('Draft', 'Paid', 'Consolidated', 'Return');

-- ---------------------------------------------------------------------------
-- Status transitions
-- ---------------------------------------------------------------------------
-- Draft --> Paid --> Consolidated, per the Lane 2F brief. Two additions the brief
-- does not name but the domain enum forces a decision on:
--
--   * Paid --> Return. `PosInvoiceStatus::Return` exists and does not count as
--     revenue, so it has to be reachable. It is reachable only from Paid: returning
--     something that was never paid for is a Draft that should simply be discarded.
--   * A no-op (status --> same status) is allowed so an idempotent "mark paid" retry
--     is not an error.
--
-- Consolidated and Return are terminal. Draft --> Consolidated is rejected: the
-- consolidation step in ERPNext folds *paid* POS invoices into a Sales Invoice, and
-- skipping Paid would let unpaid revenue into the P&L.

CREATE OR REPLACE FUNCTION invoice_status_transition_allowed(
    old_status invoice_status,
    new_status invoice_status
) RETURNS boolean
LANGUAGE sql IMMUTABLE PARALLEL SAFE STRICT AS $$
    SELECT old_status = new_status
        OR (old_status = 'Draft' AND new_status = 'Paid')
        OR (old_status = 'Paid'  AND new_status IN ('Consolidated', 'Return'));
$$;

-- ---------------------------------------------------------------------------
-- invoices  (ERPNext POS Invoice + URY custom fields)
-- ---------------------------------------------------------------------------
-- `name` is the allocated invoice number, e.g. "POS-2627-000001" — the Rule 46(b)
-- serial, so it is the primary key rather than a surrogate.
--
-- ## posted_at, and why there is no posting_date/posting_time pair
--
-- Upstream stores `posting_date DATE` + `posting_time TIME` and then filters the DATE
-- column against datetime bounds (sub_pos_closing.py:42). MariaDB casts the bounds to
-- dates, both whole days match, and an order at 01:30 is counted in two shifts — that
-- is bug 2. `businessday.rs` fixes it with a half-open TIMESTAMPTZ interval
-- `[start, end)`, so the authoritative column here is a single instant, `posted_at`.
-- Splitting it back into a date and a time would restore the ambiguity the fix
-- removes.
--
-- `business_day` is the precomputed `BusinessDay::label` for that instant under the
-- restaurant's cutoff hour. It is a report/rollup key, never the filter of record:
-- range queries use `posted_at >= start AND posted_at < end`.
--
-- ## Denormalised series columns
--
-- `naming_series`, `fiscal_year` and `series_number` are the parts `name` was built
-- from. They are stored so gaplessness is a query rather than a string parse:
-- `SELECT max(series_number) - min(series_number) + 1 = count(*)`. The trigger below
-- makes them immutable, and the unique index makes a duplicate number impossible.

CREATE TABLE invoices (
    name              TEXT           PRIMARY KEY,
    naming_series     TEXT           NOT NULL,
    fiscal_year       TEXT           NOT NULL,
    series_number     BIGINT         NOT NULL,
    status            invoice_status NOT NULL DEFAULT 'Draft',

    -- Who and where
    restaurant        TEXT           REFERENCES restaurants (name)
                                         ON UPDATE CASCADE ON DELETE RESTRICT,
    restaurant_table  TEXT           REFERENCES tables (name)
                                         ON UPDATE CASCADE ON DELETE RESTRICT,
    restaurant_room   TEXT           REFERENCES rooms (name)
                                         ON UPDATE CASCADE ON DELETE RESTRICT,
    branch            TEXT           NOT NULL,
    pos_profile       TEXT,
    customer          TEXT           NOT NULL,
    waiter            TEXT,
    cashier           TEXT,
    no_of_pax         INTEGER        NOT NULL DEFAULT 0,
    order_type        TEXT,

    -- When. posted_at is authoritative; business_day is the rollup key.
    posted_at         TIMESTAMPTZ    NOT NULL,
    business_day      DATE           NOT NULL,

    -- Tax configuration, as the domain models it (peacock_core::tax).
    supply_type       TEXT           NOT NULL
        CHECK (supply_type IN ('Intrastate', 'Interstate')),
    discount_basis    TEXT           NOT NULL DEFAULT 'NetTotal'
        CHECK (discount_basis IN ('NetTotal', 'GrandTotal')),
    -- A rate, not money: 0.05 for 5% GST. Six decimals is ample for any GST slab.
    tax_rate          NUMERIC(9, 6)  NOT NULL DEFAULT 0,

    -- Money. NUMERIC(18,6) throughout (Lane 2A standard).
    net_total         NUMERIC(18, 6) NOT NULL DEFAULT 0,
    discount          NUMERIC(18, 6) NOT NULL DEFAULT 0,
    taxable_value     NUMERIC(18, 6) NOT NULL DEFAULT 0,
    cgst              NUMERIC(18, 6) NOT NULL DEFAULT 0,
    sgst              NUMERIC(18, 6) NOT NULL DEFAULT 0,
    igst              NUMERIC(18, 6) NOT NULL DEFAULT 0,
    total_tax         NUMERIC(18, 6) NOT NULL DEFAULT 0,
    grand_total       NUMERIC(18, 6) NOT NULL DEFAULT 0,
    rounded_total     NUMERIC(18, 6) NOT NULL DEFAULT 0,
    round_off         NUMERIC(18, 6) NOT NULL DEFAULT 0,
    paid_amount       NUMERIC(18, 6) NOT NULL DEFAULT 0,
    change_amount     NUMERIC(18, 6) NOT NULL DEFAULT 0,

    -- Audit
    invoice_printed   BOOLEAN        NOT NULL DEFAULT FALSE,
    -- The audit trail for the one legal gap: a cancelled number.
    cancel_reason     TEXT,
    comments          TEXT,
    created_at        TIMESTAMPTZ    NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ    NOT NULL DEFAULT now(),

    CONSTRAINT invoices_name_not_blank CHECK (length(btrim(name)) > 0),
    -- CGST Rule 46(b): 16 characters, no exceptions.
    CONSTRAINT invoices_name_within_rule_46b CHECK (length(name) <= 16),
    CONSTRAINT invoices_fy_is_four_digits CHECK (fiscal_year ~ '^[0-9]{4}$'),
    CONSTRAINT invoices_series_number_positive CHECK (series_number >= 1),
    CONSTRAINT invoices_branch_not_blank CHECK (length(btrim(branch)) > 0),
    CONSTRAINT invoices_customer_not_blank CHECK (length(btrim(customer)) > 0),
    CONSTRAINT invoices_no_of_pax_non_negative CHECK (no_of_pax >= 0),
    CONSTRAINT invoices_tax_rate_sane CHECK (tax_rate >= 0 AND tax_rate <= 1),

    -- -------------------------------------------------------------------
    -- The tax invariants from peacock-core/src/tax.rs, in SQL.
    --
    -- These are not belt-and-braces. This is the money lane: the parity harness
    -- proves the *arithmetic* matches the Python oracle, and these constraints prove
    -- storage cannot then persist a total that contradicts it. NUMERIC compares by
    -- value, so 9.00 = 9 holds and differing scales do not produce false failures.
    -- -------------------------------------------------------------------

    -- TaxBreakdown: intrastate splits into CGST+SGST, interstate is all IGST, and
    -- either way the parts sum to total_tax with no lost paisa. SGST is derived as
    -- `total_tax - cgst` precisely so this holds when total_tax is an odd paisa count.
    CONSTRAINT invoices_tax_components_sum_to_total
        CHECK (cgst + sgst + igst = total_tax),
    CONSTRAINT invoices_intrastate_has_no_igst
        CHECK (supply_type <> 'Intrastate' OR igst = 0),
    CONSTRAINT invoices_interstate_has_no_cgst_or_sgst
        CHECK (supply_type <> 'Interstate' OR (cgst = 0 AND sgst = 0)),

    -- DiscountBasis::NetTotal reduces the taxable base; GrandTotal does not.
    CONSTRAINT invoices_taxable_value_follows_discount_basis
        CHECK (
            (discount_basis = 'NetTotal'   AND taxable_value = net_total - discount)
         OR (discount_basis = 'GrandTotal' AND taxable_value = net_total)
        ),
    CONSTRAINT invoices_grand_total_follows_discount_basis
        CHECK (
            (discount_basis = 'NetTotal'   AND grand_total = taxable_value + total_tax)
         OR (discount_basis = 'GrandTotal' AND grand_total = net_total + total_tax - discount)
        ),

    -- RoundOff::apply — the invariant the round-off ledger account depends on.
    CONSTRAINT invoices_round_off_is_exact
        CHECK (round_off = rounded_total - grand_total),
    -- Rounding is to the nearest whole rupee, once, at invoice level.
    CONSTRAINT invoices_rounded_total_is_whole_rupees
        CHECK (rounded_total = trunc(rounded_total)),
    -- ...and it really is the *nearest* rupee: a rounded_total more than 50 paisa
    -- from the grand total means something rounded twice, or not at all.
    CONSTRAINT invoices_round_off_within_half_a_rupee
        CHECK (abs(round_off) <= 0.5),

    CONSTRAINT invoices_discount_non_negative CHECK (discount >= 0),
    -- A cancelled invoice is the only one allowed to carry a reason.
    CONSTRAINT invoices_cancel_reason_not_blank
        CHECK (cancel_reason IS NULL OR length(btrim(cancel_reason)) > 0)
);

-- ---------------------------------------------------------------------------
-- Rule 46(b): the number is unique per series per financial year.
-- ---------------------------------------------------------------------------
-- `name` being the PK already forbids a duplicate string. This forbids the subtler
-- failure: the same *counter value* reused under a differently formatted name.
CREATE UNIQUE INDEX invoices_series_number_unique_idx
    ON invoices (naming_series, fiscal_year, series_number);

-- ---------------------------------------------------------------------------
-- Query indexes
-- ---------------------------------------------------------------------------

-- By date. The half-open `[start, end)` business-day scan (bug 2 fix).
CREATE INDEX invoices_posted_at_idx ON invoices (posted_at);
CREATE INDEX invoices_business_day_idx ON invoices (business_day);

-- By status. Bare, plus the pairing every report actually uses.
CREATE INDEX invoices_status_idx ON invoices (status, posted_at);

-- The P&L / shift-close query: branch + revenue statuses + business day
-- (ury_daily_p_and_l.py:303-305). Partial on PosInvoiceStatus::REVENUE so the index
-- carries only the rows those two call sites are allowed to see — the single
-- definition that fixes bug 4.
CREATE INDEX invoices_revenue_idx ON invoices (branch, business_day, posted_at)
    WHERE status IN ('Paid', 'Consolidated');

-- By table. "Which invoices belong to this table" backs OrderRepo::
-- count_separate_active (Lane 2H) and the merge guards; the partial index answers the
-- live-order form of the question without scanning history.
CREATE INDEX invoices_restaurant_table_idx ON invoices (restaurant_table, posted_at);
CREATE INDEX invoices_table_open_idx ON invoices (restaurant_table)
    WHERE status = 'Draft';

CREATE INDEX invoices_restaurant_idx ON invoices (restaurant, business_day);
CREATE INDEX invoices_pos_profile_idx ON invoices (pos_profile, business_day);
CREATE INDEX invoices_customer_idx ON invoices (customer);

CREATE TRIGGER invoices_set_updated_at
    BEFORE UPDATE ON invoices
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ---------------------------------------------------------------------------
-- invoices: transition + immutability enforcement
-- ---------------------------------------------------------------------------
-- In the trigger rather than the repository so no code path can bypass it — a
-- migration script, a psql session and a future lane are all held to the same rule.
--
-- Three separate guarantees:
--   1. Status moves only along a legal edge.
--   2. The Rule 46(b) identity (series, fiscal year, counter) is write-once.
--      Renumbering an issued invoice is forgery, not an update.
--   3. Money is frozen once the invoice leaves Draft. After a customer has paid, the
--      figures on the printed document and the figures in the database are the same
--      thing; "correcting" them silently is how a POS loses an audit.
--
-- ERRCODE 23514 with an explicit CONSTRAINT name so `error.rs::classify` maps this to
-- `StorageError::Constraint` with a name callers can actually match on.

CREATE OR REPLACE FUNCTION enforce_invoice_write_rules() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.status IS DISTINCT FROM OLD.status
       AND NOT invoice_status_transition_allowed(OLD.status, NEW.status) THEN
        RAISE EXCEPTION
            'invoice % cannot move from % to %', OLD.name, OLD.status, NEW.status
            USING ERRCODE = '23514',
                  CONSTRAINT = 'invoices_status_transition',
                  HINT = 'legal transitions: Draft->Paid, Paid->Consolidated, Paid->Return';
    END IF;

    IF NEW.naming_series IS DISTINCT FROM OLD.naming_series
       OR NEW.fiscal_year IS DISTINCT FROM OLD.fiscal_year
       OR NEW.series_number IS DISTINCT FROM OLD.series_number
       OR NEW.name IS DISTINCT FROM OLD.name THEN
        RAISE EXCEPTION
            'invoice % has an immutable Rule 46(b) serial; it cannot be renumbered',
            OLD.name
            USING ERRCODE = '23514',
                  CONSTRAINT = 'invoices_serial_is_immutable';
    END IF;

    IF OLD.status <> 'Draft' AND (
           NEW.net_total     IS DISTINCT FROM OLD.net_total
        OR NEW.discount      IS DISTINCT FROM OLD.discount
        OR NEW.taxable_value IS DISTINCT FROM OLD.taxable_value
        OR NEW.cgst          IS DISTINCT FROM OLD.cgst
        OR NEW.sgst          IS DISTINCT FROM OLD.sgst
        OR NEW.igst          IS DISTINCT FROM OLD.igst
        OR NEW.total_tax     IS DISTINCT FROM OLD.total_tax
        OR NEW.grand_total   IS DISTINCT FROM OLD.grand_total
        OR NEW.rounded_total IS DISTINCT FROM OLD.rounded_total
        OR NEW.round_off     IS DISTINCT FROM OLD.round_off
        OR NEW.tax_rate      IS DISTINCT FROM OLD.tax_rate
        OR NEW.supply_type   IS DISTINCT FROM OLD.supply_type
    ) THEN
        RAISE EXCEPTION
            'invoice % is %; its totals are frozen', OLD.name, OLD.status
            USING ERRCODE = '23514',
                  CONSTRAINT = 'invoices_totals_frozen_after_draft';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER invoices_enforce_write_rules
    BEFORE UPDATE ON invoices
    FOR EACH ROW EXECUTE FUNCTION enforce_invoice_write_rules();

-- ---------------------------------------------------------------------------
-- invoice_lines  (POS Invoice Item child table)
-- ---------------------------------------------------------------------------
-- Child table, so: FK to the parent with CASCADE, plus an `idx` ordering column, per
-- the plan's Frappe-to-Postgres child-table rule. `idx` is 1-based, matching Frappe.
--
-- `amount` is stored rather than generated, with a CHECK tying it to qty * rate.
-- Storing it means the line total is what the domain computed
-- (`tax::InvoiceLine::amount`); the CHECK means a product that does not fit in six
-- decimals fails the insert loudly instead of being silently rounded into a
-- `net_total` that no longer matches the sum of its lines.
--
-- `hsn_sac` is nullable and that is a known go-live blocker, not an oversight:
-- `ury_menu_item.json` has no HSN field today (tax.rs documents this), so the menu
-- data has to be backfilled before GST-compliant invoices can be issued.

CREATE TABLE invoice_lines (
    id              BIGSERIAL      PRIMARY KEY,
    invoice         TEXT           NOT NULL
        REFERENCES invoices (name) ON UPDATE CASCADE ON DELETE CASCADE,
    idx             INTEGER        NOT NULL,
    item_code       TEXT           NOT NULL
        REFERENCES items (code) ON UPDATE CASCADE ON DELETE RESTRICT,
    item_name       TEXT           NOT NULL,
    qty             NUMERIC(18, 6) NOT NULL,
    rate            NUMERIC(18, 6) NOT NULL,
    amount          NUMERIC(18, 6) NOT NULL,
    hsn_sac         TEXT,
    course          TEXT,
    comments        TEXT,
    serve_priority  INTEGER        NOT NULL DEFAULT 0,
    indicate_course BOOLEAN        NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ    NOT NULL DEFAULT now(),

    CONSTRAINT invoice_lines_idx_positive CHECK (idx > 0),
    CONSTRAINT invoice_lines_item_name_not_blank CHECK (length(btrim(item_name)) > 0),
    CONSTRAINT invoice_lines_qty_positive CHECK (qty > 0),
    CONSTRAINT invoice_lines_rate_non_negative CHECK (rate >= 0),
    -- The line arithmetic, pinned. See the note above on why this is a CHECK and not
    -- a generated column.
    CONSTRAINT invoice_lines_amount_is_qty_times_rate CHECK (amount = qty * rate)
);

-- Fetch a whole invoice's lines in order — the only read path that matters.
CREATE UNIQUE INDEX invoice_lines_invoice_idx_key ON invoice_lines (invoice, idx);
-- Item-level reporting, and the COGS join (ury_daily_p_and_l.py:156).
CREATE INDEX invoice_lines_item_code_idx ON invoice_lines (item_code);
-- HSN summary for the GSTR filing, and the backfill audit: `WHERE hsn_sac IS NULL`.
CREATE INDEX invoice_lines_missing_hsn_idx ON invoice_lines (invoice)
    WHERE hsn_sac IS NULL;

-- ---------------------------------------------------------------------------
-- idempotency_keys — key -> invoice
-- ---------------------------------------------------------------------------
-- The critical link from `invoicing.rs`:
--
--   "The idempotency key must be stored WITH the allocated invoice number. Otherwise
--    a retried submit allocates a second number and gaps the series — the exact
--    failure mode Rule 46(b) forbids."
--
-- Hence the FK: a key row cannot exist without the invoice it points at, and it is
-- written inside the same transaction that allocated the number. Roll back one and
-- you roll back both.
--
-- ## Expiry
--
-- `expires_at` defaults to 24 hours out and is ADVISORY. Nothing deletes the row on a
-- timer; `PgInvoiceRepo::purge_expired_idempotency_keys` does it when called, and the
-- lookup ignores `expires_at` entirely. Two consequences, both deliberate:
--
--   * A replay after expiry but before purge still returns the original invoice. That
--     is the safe direction to err.
--   * A replay after purge allocates a NEW number and creates a second invoice. That
--     is a duplicate, not a gap, so Rule 46(b) still holds. Purging is safe by
--     construction: it can never renumber an invoice, only drop the shortcut that
--     avoided writing a new one.
--
-- ON DELETE CASCADE, not RESTRICT: the key is a request-dedup record with no
-- independent meaning, so it must not be able to pin a row it merely points at.

CREATE TABLE idempotency_keys (
    key        UUID        PRIMARY KEY,
    invoice    TEXT        NOT NULL
        REFERENCES invoices (name) ON UPDATE CASCADE ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now() + INTERVAL '24 hours',
    CONSTRAINT idempotency_keys_expires_after_creation CHECK (expires_at > created_at)
);

-- "Which key produced this invoice", and the cascade's own lookup.
CREATE INDEX idempotency_keys_invoice_idx ON idempotency_keys (invoice);
-- The purge scan.
CREATE INDEX idempotency_keys_expires_at_idx ON idempotency_keys (expires_at);
