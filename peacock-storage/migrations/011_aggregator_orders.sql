-- Lane W1-D: Aggregator order integration (Swiggy/Zomato webhooks).
--
-- Aggregator orders are received via webhook, stored as pending, then accepted (creating
-- an internal order + invoice + KOT) or rejected. Settlement reconciliation queries
-- completed orders to match against platform payouts.

-- ---------------------------------------------------------------------------
-- aggregator_order_status enum
-- ---------------------------------------------------------------------------

CREATE TYPE aggregator_order_status AS ENUM ('Pending', 'Accepted', 'Rejected', 'Completed');

-- ---------------------------------------------------------------------------
-- aggregator_orders
-- ---------------------------------------------------------------------------

CREATE TABLE aggregator_orders (
    id                     TEXT           PRIMARY KEY,
    aggregator_order_id    TEXT           NOT NULL UNIQUE,
    platform               TEXT           NOT NULL,
    customer_name          TEXT           NOT NULL,
    customer_phone         TEXT,
    total                  NUMERIC(18, 6) NOT NULL,
    ordered_at             TIMESTAMPTZ    NOT NULL,
    status                 aggregator_order_status NOT NULL DEFAULT 'Pending',
    internal_order_id      BIGINT         REFERENCES orders (id)
                                              ON UPDATE CASCADE ON DELETE SET NULL,
    internal_invoice_id    TEXT           REFERENCES invoices (name)
                                              ON UPDATE CASCADE ON DELETE SET NULL,
    instructions           TEXT,
    reject_reason          TEXT,
    created_at             TIMESTAMPTZ    NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ    NOT NULL DEFAULT now(),

    CONSTRAINT aggregator_orders_id_not_blank CHECK (length(btrim(id)) > 0),
    CONSTRAINT aggregator_orders_aggregator_order_id_not_blank
        CHECK (length(btrim(aggregator_order_id)) > 0),
    CONSTRAINT aggregator_orders_platform_not_blank CHECK (length(btrim(platform)) > 0),
    CONSTRAINT aggregator_orders_customer_name_not_blank
        CHECK (length(btrim(customer_name)) > 0),
    CONSTRAINT aggregator_orders_total_non_negative CHECK (total >= 0),
    CONSTRAINT aggregator_orders_status_transitions
        CHECK (
            -- Pending can become Accepted or Rejected
            -- Accepted can become Completed
            -- Rejected is terminal
            (status = 'Pending')
            OR (status = 'Accepted' AND internal_order_id IS NOT NULL AND internal_invoice_id IS NOT NULL)
            OR (status = 'Rejected' AND reject_reason IS NOT NULL)
            OR (status = 'Completed' AND internal_order_id IS NOT NULL AND internal_invoice_id IS NOT NULL)
        )
);

CREATE INDEX aggregator_orders_platform_idx ON aggregator_orders (platform);
CREATE INDEX aggregator_orders_status_idx ON aggregator_orders (status);
CREATE INDEX aggregator_orders_ordered_at_idx ON aggregator_orders (ordered_at);
CREATE INDEX aggregator_orders_internal_invoice_idx ON aggregator_orders (internal_invoice_id)
    WHERE internal_invoice_id IS NOT NULL;

CREATE TRIGGER aggregator_orders_set_updated_at
    BEFORE UPDATE ON aggregator_orders
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ---------------------------------------------------------------------------
-- aggregator_order_items (child table)
-- ---------------------------------------------------------------------------

CREATE TABLE aggregator_order_items (
    id                     BIGSERIAL      PRIMARY KEY,
    aggregator_order_id    TEXT           NOT NULL REFERENCES aggregator_orders (id)
                                              ON UPDATE CASCADE ON DELETE CASCADE,
    item_code              TEXT           NOT NULL,
    item_name              TEXT           NOT NULL,
    quantity               NUMERIC(18, 6) NOT NULL,
    rate                   NUMERIC(18, 6) NOT NULL,
    special_instructions   TEXT,

    CONSTRAINT aggregator_order_items_item_code_not_blank
        CHECK (length(btrim(item_code)) > 0),
    CONSTRAINT aggregator_order_items_item_name_not_blank
        CHECK (length(btrim(item_name)) > 0),
    CONSTRAINT aggregator_order_items_quantity_positive CHECK (quantity > 0),
    CONSTRAINT aggregator_order_items_rate_non_negative CHECK (rate >= 0)
);

CREATE INDEX aggregator_order_items_order_idx ON aggregator_order_items (aggregator_order_id);

-- ---------------------------------------------------------------------------
-- aggregator_settlements (for reconciliation)
-- ---------------------------------------------------------------------------

CREATE TABLE aggregator_settlements (
    id                TEXT           PRIMARY KEY,
    platform          TEXT           NOT NULL,
    settlement_date   DATE           NOT NULL,
    total_orders      INTEGER        NOT NULL,
    gross_amount      NUMERIC(18, 6) NOT NULL,
    commission        NUMERIC(18, 6) NOT NULL,
    net_amount        NUMERIC(18, 6) NOT NULL,
    created_at        TIMESTAMPTZ    NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ    NOT NULL DEFAULT now(),

    CONSTRAINT aggregator_settlements_id_not_blank CHECK (length(btrim(id)) > 0),
    CONSTRAINT aggregator_settlements_platform_not_blank CHECK (length(btrim(platform)) > 0),
    CONSTRAINT aggregator_settlements_total_orders_non_negative CHECK (total_orders >= 0),
    CONSTRAINT aggregator_settlements_gross_non_negative CHECK (gross_amount >= 0),
    CONSTRAINT aggregator_settlements_commission_non_negative CHECK (commission >= 0),
    CONSTRAINT aggregator_settlements_net_non_negative CHECK (net_amount >= 0)
);

CREATE INDEX aggregator_settlements_platform_date_idx
    ON aggregator_settlements (platform, settlement_date);

CREATE TRIGGER aggregator_settlements_set_updated_at
    BEFORE UPDATE ON aggregator_settlements
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ---------------------------------------------------------------------------
-- aggregator_settlement_orders (junction table)
-- ---------------------------------------------------------------------------

CREATE TABLE aggregator_settlement_orders (
    settlement_id         TEXT NOT NULL REFERENCES aggregator_settlements (id)
                               ON UPDATE CASCADE ON DELETE CASCADE,
    aggregator_order_id   TEXT NOT NULL REFERENCES aggregator_orders (aggregator_order_id)
                               ON UPDATE CASCADE ON DELETE CASCADE,
    PRIMARY KEY (settlement_id, aggregator_order_id)
);

CREATE INDEX aggregator_settlement_orders_order_idx
    ON aggregator_settlement_orders (aggregator_order_id);
