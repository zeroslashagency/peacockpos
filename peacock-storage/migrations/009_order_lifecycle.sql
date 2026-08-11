-- Lane 4A-1: the order lifecycle columns the HTTP layer needs.
--
-- 007_order.sql modelled `orders` as pure transient waiter-session state, which is what
-- upstream's Single doctype is. The API adds two things on top of that which the form
-- itself has no field for, and both have to be in the database rather than in a process:
--
--   1. **Cancellation.** `DELETE /api/orders/:id` is a *soft* cancel — the row stays for
--      the audit trail and the order becomes unmodifiable. There is no upstream status
--      field to reuse (GROUND-TRUTH.md; ury_order.json has none), and the API's
--      `OrderStatus` is deliberately an API-boundary concept (`dto/order.rs`). So the
--      one bit the database must own is *when* it was cancelled, and the rest of the
--      status is derived: `last_invoice IS NOT NULL` → invoiced, `cancelled_at IS NOT
--      NULL` → cancelled, otherwise open.
--
--   2. **Idempotent creation.** `POST /api/orders` honours an `Idempotency-Key`, and a
--      replay must return the *original* order. Holding that map in memory would make
--      the guarantee last exactly as long as the process: a retry that arrives after a
--      restart, or on a second terminal behind a load balancer, would create a second
--      order and a second bill. 005_invoice.sql already makes this argument for invoice
--      numbers; the same reasoning applies one level up.
--
-- ## Why a separate key table rather than reusing `idempotency_keys`
--
-- `idempotency_keys.invoice` is `NOT NULL REFERENCES invoices (name)`. An order key
-- points at an `orders.id`, which is a BIGINT and may never become an invoice at all, so
-- it cannot live in that table without making the FK nullable and the table mean two
-- things. Separate table, same shape, same advisory-expiry reasoning.

-- ---------------------------------------------------------------------------
-- orders.cancelled_at
-- ---------------------------------------------------------------------------
-- Nullable TIMESTAMPTZ rather than a boolean: "cancelled at 23:14" is strictly more
-- information than "cancelled", it costs the same, and the audit trail wants the time.
ALTER TABLE orders
    ADD COLUMN cancelled_at TIMESTAMPTZ;

-- The reason, when the operator gave one. Same rule as `invoices.cancel_reason`: present
-- or absent, never blank, so a UI can trust that a non-NULL value says something.
ALTER TABLE orders
    ADD COLUMN cancel_reason TEXT;

ALTER TABLE orders
    ADD CONSTRAINT orders_cancel_reason_not_blank
    CHECK (cancel_reason IS NULL OR length(btrim(cancel_reason)) > 0);

-- A reason without a cancellation is a contradiction: it would claim the order was
-- voided while every read path still reports it open.
ALTER TABLE orders
    ADD CONSTRAINT orders_cancel_reason_requires_cancellation
    CHECK (cancel_reason IS NULL OR cancelled_at IS NOT NULL);

-- ---------------------------------------------------------------------------
-- "One live form per table" has to stop counting cancelled forms
-- ---------------------------------------------------------------------------
-- 007_order.sql's `orders_one_live_form_per_table_idx` is unconditional on
-- `restaurant_table`, which was right when a form was deleted at settle time. Once a
-- cancelled form *stays* for the audit trail, that index means table T-01 can never be
-- opened again: the first cancelled row occupies the slot permanently.
--
-- Cancelled forms are therefore excluded. Invoiced forms are NOT: an order that raised
-- an invoice still owns its table until the bill is settled and the form is deleted, and
-- letting a second cart open on top of it is how a table ends up with two bills.
DROP INDEX orders_one_live_form_per_table_idx;

CREATE UNIQUE INDEX orders_one_live_form_per_table_idx
    ON orders (restaurant_table)
    WHERE restaurant_table IS NOT NULL AND cancelled_at IS NULL;

-- The cancelled-order audit sweep, and the "still open" listing that is its complement.
CREATE INDEX orders_cancelled_at_idx ON orders (cancelled_at)
    WHERE cancelled_at IS NOT NULL;

-- ---------------------------------------------------------------------------
-- order_idempotency_keys — key -> orders.id
-- ---------------------------------------------------------------------------
-- Written inside the same transaction that inserts the order, so the key and the row it
-- names commit together or not at all. A key that survived a rolled-back insert would
-- send every later replay to an order that does not exist.
--
-- ON DELETE CASCADE: the key is a request-dedup record with no independent meaning, so
-- it must not be able to pin the row it merely points at. Same call as
-- `idempotency_keys` in 005_invoice.sql.
--
-- `expires_at` is advisory, exactly as it is for invoice keys: nothing deletes on a
-- timer, the lookup ignores it, and a purge can only cost a future replay a fresh order
-- — never renumber or duplicate an existing one.
CREATE TABLE order_idempotency_keys (
    key        UUID        PRIMARY KEY,
    order_id   BIGINT      NOT NULL
        REFERENCES orders (id) ON UPDATE CASCADE ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL DEFAULT now() + INTERVAL '24 hours',
    CONSTRAINT order_idempotency_keys_expires_after_creation
        CHECK (expires_at > created_at)
);

-- "Which key produced this order", and the cascade's own lookup.
CREATE INDEX order_idempotency_keys_order_id_idx ON order_idempotency_keys (order_id);
-- The purge scan.
CREATE INDEX order_idempotency_keys_expires_at_idx ON order_idempotency_keys (expires_at);
