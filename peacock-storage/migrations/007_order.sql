-- Lane 2H: the order form.
--
-- ## This table is not the order of record
--
-- `URY Order` (ury_order.json) is a Frappe **UI form**: `"issingle": 1`, no docname,
-- no status field, no tax or payment fields, and half its `field_order` is screen
-- furniture (`table_tab`, `menu_tab`, `cart_items` HTML, `favorite_items` HTML).
-- peacock_core::model::UryOrderForm says the same thing in its doc comment. The real
-- record is the POS Invoice, reached through `last_invoice` (Lane 2F, 005_invoice.sql).
--
-- What that means for this schema:
--   * Rows here are **transient waiter-session state**, not history. They are deleted
--     when the bill is settled; nothing downstream reads them for reporting, so there
--     is no soft-delete column and no status enum.
--   * Money on this table is a *display* figure. `grand_total` is `read_only` upstream
--     and recomputed from the invoice, so it is never a tax basis. It is still NUMERIC,
--     never float — money and float do not mix anywhere in this codebase
--     (peacock-core/src/money.rs).
--   * `OrderRepo::count_separate_active` does **not** read this table. It counts
--     unprinted draft POS Invoices (`_table_has_active_order`, ury_order.py:223-233),
--     which live in `invoices`. See peacock-storage/src/repos/order.rs.
--
-- ## Identity
--
-- Upstream is a Single doctype, so there is no docname to carry over and the Lane 2A
-- "every primary key is TEXT" rule does not apply — there is no user-visible name to
-- preserve. `id BIGSERIAL`, like `shifts.id` in 006_shift.sql.
--
-- ## Concurrency
--
-- Two waiters on two tablets can hit the same table's form at the same time. The
-- repository takes a `SELECT ... FOR UPDATE` row lock for the whole read-modify-write,
-- so the second waiter blocks on the first rather than overwriting them, and `version`
-- makes a lost update detectable after the fact instead of silent.

-- ---------------------------------------------------------------------------
-- orders  (URY Order — the UI form binding)
-- ---------------------------------------------------------------------------
-- Field-for-field against peacock_core::model::UryOrderForm:
--   take_away, restaurant_table, customer_name, no_of_pax, grand_total, last_invoice,
--   items (→ order_items), waiter, pos_profile, cashier, comments, modified_time.
--
-- `modified_time` is a real upstream field (a `Datetime` the POS writes for optimistic
-- reload), and is deliberately separate from the `updated_at` trigger column: one is
-- application state the client sends, the other is row bookkeeping the database owns.

CREATE TABLE orders (
    id               BIGSERIAL PRIMARY KEY,

    -- The table binding. `depends_on: eval:doc.restaurant_table || doc.take_away`
    -- upstream, so a form has to be bound to something to exist at all — see the
    -- orders_has_a_binding CHECK.
    take_away        BOOLEAN        NOT NULL DEFAULT FALSE,
    restaurant_table TEXT
        REFERENCES tables (name) ON UPDATE CASCADE ON DELETE RESTRICT,

    -- `customer_name` and `no_of_pax` are `reqd: 1` upstream (ury_order.json).
    customer_name    TEXT           NOT NULL,
    no_of_pax        INTEGER        NOT NULL DEFAULT 1,

    -- Display total, recomputed from the invoice. NUMERIC(18,6) rather than (18,2) so a
    -- round trip cannot quietly re-round a `Money` the domain already rounded; rounding
    -- is money.rs's job and it must happen exactly once.
    grand_total      NUMERIC(18, 6) NOT NULL DEFAULT 0,

    -- FK added below, once `invoices` exists.
    last_invoice     TEXT,

    waiter           TEXT,
    pos_profile      TEXT,
    cashier          TEXT,
    comments         TEXT,
    modified_time    TIMESTAMPTZ,

    -- Bumped by every repository update inside the row lock. A caller that reads
    -- version N, is overtaken, and writes expecting N gets Error::Conflict rather than
    -- clobbering the other waiter.
    version          BIGINT         NOT NULL DEFAULT 1,

    created_at       TIMESTAMPTZ    NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ    NOT NULL DEFAULT now(),

    CONSTRAINT orders_customer_name_not_blank CHECK (length(btrim(customer_name)) > 0),
    CONSTRAINT orders_no_of_pax_positive CHECK (no_of_pax > 0),
    CONSTRAINT orders_grand_total_non_negative CHECK (grand_total >= 0),
    CONSTRAINT orders_version_positive CHECK (version > 0),
    -- A form with neither a table nor the take-away flag is unreachable in the UI, so
    -- it can only be a bug or a stray write.
    CONSTRAINT orders_has_a_binding CHECK (take_away OR restaurant_table IS NOT NULL)
);

-- One live form per table. This is what makes "the form for table T-01" a single row
-- the repository can lock, and it turns a double-open race into a 23505 instead of two
-- rival carts. Take-away forms carry no table, and NULLs are distinct, so any number of
-- them can coexist.
CREATE UNIQUE INDEX orders_one_live_form_per_table_idx
    ON orders (restaurant_table) WHERE restaurant_table IS NOT NULL;

-- "Which form points at this invoice" — the reverse of last_invoice, used when an
-- invoice is settled and the form has to be cleared.
CREATE INDEX orders_last_invoice_idx ON orders (last_invoice) WHERE last_invoice IS NOT NULL;

-- Waiter's own open forms; the POS home screen query.
CREATE INDEX orders_waiter_idx ON orders (waiter) WHERE waiter IS NOT NULL;

-- Take-away queue.
CREATE INDEX orders_take_away_idx ON orders (take_away, created_at) WHERE take_away;

CREATE TRIGGER orders_set_updated_at
    BEFORE UPDATE ON orders
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ---------------------------------------------------------------------------
-- order_items  (ury_order_item child table)
-- ---------------------------------------------------------------------------
-- Child-table rule from the plan: FK to the parent plus an `idx` ordering column,
-- because `UryOrderForm::items` is a Vec and the cart's order is what the waiter sees.
--
-- `qty` is `Int` upstream and the domain mirrors that (`OrderItem::qty: i32`), so it is
-- INTEGER here. That means 0.5 kg is not representable — a real limitation, flagged in
-- model.rs, and changing it is a schema change rather than a port.

-- ---------------------------------------------------------------------------
-- orders.last_invoice → invoices  (the Lane 2F dependency)
-- ---------------------------------------------------------------------------
-- ## It references invoices(name), not invoices(id)
--
-- The Lane 2H brief says "FK to `invoices.id`". There is no such column: Lane 2F made
-- the Rule 46(b) serial itself the primary key (`invoices.name TEXT PRIMARY KEY`,
-- 005_invoice.sql) rather than adding a surrogate, and the domain agrees — `last_invoice`
-- is `Option<InvoiceName>`, a newtype over String. A surrogate FK here would force the
-- repository to join just to hand the UI the invoice number it already had.
--
-- ## ON DELETE SET NULL, not RESTRICT
--
-- Both were on the table. SET NULL wins:
--
--   * An issued invoice is a tax document; 005_invoice.sql deliberately gives it no
--     soft-delete column, and its trigger refuses to let the serial be renumbered. So
--     in normal operation this cascade never fires at all, and the choice only decides
--     what happens during a deliberate administrative purge or a test teardown.
--   * When it does fire, RESTRICT would let a stale *UI form* — transient waiter-session
--     state, deleted at settle time — block the removal of a row in the ledger. That
--     inverts the dependency: the form points at the invoice, never the reverse, and the
--     junior record must not pin the senior one.
--   * SET NULL leaves the form in the state it already has a meaning for: `last_invoice`
--     is nullable and `None` is simply "no invoice raised yet". No orphan, no NOT NULL
--     violation, nothing for the repository to special-case.
--
-- The corollary is that losing the pointer must not lose money, and it does not: the
-- invoice is the order of record, so a cleared `last_invoice` costs a UI convenience
-- link, not a row of revenue.
--
-- ON UPDATE CASCADE is stated for symmetry with the rest of the schema. It is
-- unreachable in practice: `invoices_serial_is_immutable` rejects any rename.

ALTER TABLE orders
    ADD CONSTRAINT orders_last_invoice_fkey
    FOREIGN KEY (last_invoice) REFERENCES invoices (name)
    ON UPDATE CASCADE ON DELETE SET NULL;

CREATE TABLE order_items (
    id         BIGSERIAL PRIMARY KEY,
    order_id   BIGINT         NOT NULL
        REFERENCES orders (id) ON UPDATE CASCADE ON DELETE CASCADE,
    idx        INTEGER        NOT NULL,
    item       TEXT           NOT NULL
        REFERENCES items (code) ON UPDATE CASCADE ON DELETE RESTRICT,
    item_name  TEXT           NOT NULL,
    qty        INTEGER        NOT NULL,
    rate       NUMERIC(18, 6) NOT NULL,
    comments   TEXT,
    created_at TIMESTAMPTZ    NOT NULL DEFAULT now(),
    CONSTRAINT order_items_idx_positive CHECK (idx > 0),
    CONSTRAINT order_items_qty_positive CHECK (qty > 0),
    CONSTRAINT order_items_rate_non_negative CHECK (rate >= 0)
);

-- The cart read: every line of a form, in cart order, one index scan.
CREATE UNIQUE INDEX order_items_order_idx ON order_items (order_id, idx);

-- "Which open carts contain this item" — the 86 / out-of-stock sweep.
CREATE INDEX order_items_item_idx ON order_items (item);
