-- ---------------------------------------------------------------------------
-- 008_invoice_payments.sql — Lane 4A-3. **Money lane.**
-- ---------------------------------------------------------------------------
-- `POST /api/invoices/:id/pay` accumulates split tender against one invoice, and
-- `invoices.paid_amount` is a single NUMERIC column. A cashier taking ₹300 card then
-- ₹78 cash has to leave two rows behind, not one overwritten total: the Z-report
-- splits the drawer by instrument (CGST Rule 56's ₹10k cash threshold is per
-- instrument, not per invoice), and a single column cannot answer "how much of this
-- was cash".
--
-- So payments get their own table, and `invoices.paid_amount` becomes a cache of
-- their sum, maintained by trigger. Two consequences, both wanted:
--
--   * No handler can compute `paid_amount` itself and get it wrong. The database
--     derives it from the rows that justify it.
--   * `outstanding_amount` is `rounded_total - paid_amount` and is therefore exact by
--     construction, never a figure two code paths could disagree about.
--
-- ## Why rounded_total and not grand_total
--
-- `businessday.rs` bug 3: upstream settled against `grand_total` while the customer
-- paid `rounded_total`, leaving a sub-rupee residue on every cash bill that never
-- reconciled. The overpayment CHECK below uses `rounded_total` — the figure actually
-- tendered.

-- ---------------------------------------------------------------------------
-- Payment instrument
-- ---------------------------------------------------------------------------
-- Mirrors `peacock_api::dto::invoice::PaymentMethodDto`, which in turn mirrors the
-- ERPNext `Mode of Payment` values URY ships. An ENUM rather than free TEXT: the
-- Z-report branches on `Cash` to compute the drawer total, and a typo'd 'cash' would
-- silently drop a note out of the cash column and out of the Rule 56 threshold check.

CREATE TYPE payment_method AS ENUM ('Cash', 'Card', 'Upi', 'Wallet', 'Credit');

-- ---------------------------------------------------------------------------
-- invoice_payments
-- ---------------------------------------------------------------------------
-- `idx` is 1-based and ordered, matching every other child table in this schema
-- (`invoice_lines`, `kot_items`) so "the second tender on this bill" is answerable.
--
-- ON DELETE CASCADE: a payment has no meaning without its invoice. Invoices are never
-- hard-deleted in normal operation (a cancellation carries `cancel_reason` and the row
-- stays, per 005), so this only fires on an explicit administrative purge.

CREATE TABLE invoice_payments (
    id         BIGSERIAL      PRIMARY KEY,
    invoice    TEXT           NOT NULL
        REFERENCES invoices (name) ON UPDATE CASCADE ON DELETE CASCADE,
    idx        INTEGER        NOT NULL,
    method     payment_method NOT NULL,
    -- NUMERIC(18,6), the Lane 2A money standard. Never float.
    amount     NUMERIC(18, 6) NOT NULL,
    -- Card/UPI transaction reference. NULL for cash, which has none.
    reference  TEXT,
    paid_at    TIMESTAMPTZ    NOT NULL,
    created_at TIMESTAMPTZ    NOT NULL DEFAULT now(),

    CONSTRAINT invoice_payments_idx_positive CHECK (idx > 0),
    -- A zero payment is not a tender, and a negative one is a refund, which is what
    -- the Return status is for. Both are rejected rather than quietly stored.
    CONSTRAINT invoice_payments_amount_positive CHECK (amount > 0),
    CONSTRAINT invoice_payments_reference_not_blank
        CHECK (reference IS NULL OR length(btrim(reference)) > 0)
);

CREATE UNIQUE INDEX invoice_payments_invoice_idx_key ON invoice_payments (invoice, idx);
-- "Every payment on this bill, in order" — the read path behind InvoiceResponse.
CREATE INDEX invoice_payments_invoice_key ON invoice_payments (invoice);
-- The Z-report's drawer split: sum by instrument over a time window.
CREATE INDEX invoice_payments_method_paid_at_idx ON invoice_payments (method, paid_at);

-- ---------------------------------------------------------------------------
-- paid_amount, derived
-- ---------------------------------------------------------------------------
-- The trigger recomputes from the rows rather than adding a delta. A delta is one
-- missed UPDATE away from drifting, and a drifted `paid_amount` is a bill that either
-- refuses a legitimate final payment or accepts one too many.
--
-- SECURITY: `SET search_path = pg_catalog, public` pins name resolution. Without it a
-- caller who can create a temporary `invoice_payments` relation could shadow the real
-- one for the duration of the trigger and reroute the money sum.

CREATE OR REPLACE FUNCTION invoice_payments_refresh_paid_amount()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog, public
AS $$
DECLARE
    target TEXT := COALESCE(NEW.invoice, OLD.invoice);
BEGIN
    UPDATE invoices
       SET paid_amount = COALESCE(
               (SELECT sum(amount) FROM invoice_payments WHERE invoice = target),
               0
           )
     WHERE name = target;
    RETURN NULL;
END;
$$;

-- AFTER, and statement-level per row change: the sum has to see the committed set of
-- sibling rows, which a BEFORE trigger would not.
CREATE TRIGGER invoice_payments_maintain_paid_amount
    AFTER INSERT OR UPDATE OR DELETE ON invoice_payments
    FOR EACH ROW EXECUTE FUNCTION invoice_payments_refresh_paid_amount();

-- ---------------------------------------------------------------------------
-- The overpayment guard
-- ---------------------------------------------------------------------------
-- A POS that accepts more than the bill produces a negative outstanding figure that
-- reconciles against nothing. Enforced in the database as well as the handler because
-- the handler's read-then-write can lose a race: two concurrent ₹300 payments on a
-- ₹378 bill both see ₹0 paid, both pass the application check, and the second one has
-- to fail here or the invoice ends up ₹222 over.
--
-- Deferred to the end of the statement so the paid_amount trigger has already run.

CREATE OR REPLACE FUNCTION invoice_payments_reject_overpayment()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog, public
AS $$
DECLARE
    target   TEXT := COALESCE(NEW.invoice, OLD.invoice);
    settled  NUMERIC(18, 6);
    due      NUMERIC(18, 6);
BEGIN
    SELECT COALESCE(sum(amount), 0) INTO settled
      FROM invoice_payments WHERE invoice = target;

    -- FOR KEY SHARE, not a bare read: it blocks a concurrent transaction from
    -- retiring the invoice under us without blocking sibling payment inserts, which
    -- serialise on their own unique (invoice, idx) index anyway.
    SELECT rounded_total INTO due
      FROM invoices WHERE name = target FOR KEY SHARE;

    IF due IS NOT NULL AND settled > due THEN
        RAISE EXCEPTION
            'payments on invoice % total %, which exceeds its rounded_total of %',
            target, settled, due
            USING ERRCODE = 'check_violation',
                  CONSTRAINT = 'invoice_payments_within_rounded_total';
    END IF;

    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER invoice_payments_enforce_no_overpayment
    AFTER INSERT OR UPDATE ON invoice_payments
    DEFERRABLE INITIALLY IMMEDIATE
    FOR EACH ROW EXECUTE FUNCTION invoice_payments_reject_overpayment();
