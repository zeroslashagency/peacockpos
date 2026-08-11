-- Lane 2G: Shift management.
--
-- A shift is a business day's operating session (typically open at start of day,
-- closed at end of day with a Z-report). Each shift belongs to a terminal (POS
-- Profile in ERPNext terms) and has start/end timestamps, cash/card totals, and
-- a cash deposit tracking flag (CGST Rule 56: cash over ₹10k/day must be deposited
-- by next banking day).
--
-- Business day calculation (businessday.rs) fixes bug 2: orders before the cutoff
-- hour (e.g., 03:00 IST) belong to the previous calendar day's business day, so a
-- dinner shift crossing midnight correctly buckets all its invoices without double-
-- counting.

-- ---------------------------------------------------------------------------
-- shifts
-- ---------------------------------------------------------------------------
-- Key constraints:
--   * Only one open shift per terminal at a time (UNIQUE partial index on terminal + status)
--   * Shift spans a business day: [opened_at, closed_at), using the restaurant's
--     cutoff hour (stored separately, e.g., in a restaurant_settings table or passed
--     at query time). The half-open interval prevents bug 2 double-counting.
--   * Z-report data: cash_total, card_total, invoice_count (verified against actual
--     invoices at close time).
--   * Cash deposit tracking: cash_over_threshold flag + deposit_verified timestamp
--     (CGST Rule 56 compliance).

CREATE TABLE shifts (
    id                    BIGSERIAL PRIMARY KEY,
    terminal              TEXT        NOT NULL,  -- POS Profile name
    opened_at             TIMESTAMPTZ NOT NULL,
    closed_at             TIMESTAMPTZ,
    business_day_label    DATE        NOT NULL,  -- Calendar date this shift represents
    cutoff_hour           INTEGER     NOT NULL CHECK (cutoff_hour >= 0 AND cutoff_hour < 24),
    status                TEXT        NOT NULL CHECK (status IN ('open', 'closed')),
    
    -- Z-report data (populated at close)
    cash_total            NUMERIC(18, 2) DEFAULT 0,
    card_total            NUMERIC(18, 2) DEFAULT 0,
    invoice_count         INTEGER        DEFAULT 0,
    
    -- CGST Rule 56: ₹10k/day cash deposit requirement
    cash_over_threshold   BOOLEAN     NOT NULL DEFAULT FALSE,
    deposit_verified_at   TIMESTAMPTZ,
    
    -- Audit fields
    opened_by             TEXT,       -- user who opened the shift
    closed_by             TEXT,       -- user who closed the shift
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    
    CONSTRAINT shifts_closed_at_after_opened CHECK (closed_at IS NULL OR closed_at >= opened_at),
    CONSTRAINT shifts_closed_requires_totals CHECK (
        status = 'open' OR (
            status = 'closed' AND
            closed_at IS NOT NULL AND
            cash_total IS NOT NULL AND
            card_total IS NOT NULL AND
            invoice_count IS NOT NULL
        )
    ),
    CONSTRAINT shifts_deposit_verified_requires_threshold CHECK (
        deposit_verified_at IS NULL OR cash_over_threshold = TRUE
    )
);

-- Only one open shift per terminal (enforces "shift already open" error)
CREATE UNIQUE INDEX shifts_one_open_per_terminal_idx
    ON shifts (terminal) WHERE status = 'open';

-- Query current shift for a terminal
CREATE INDEX shifts_terminal_status_idx ON shifts (terminal, status, opened_at DESC);

-- Query shifts by business day (for P&L aggregation)
CREATE INDEX shifts_business_day_idx ON shifts (business_day_label);

-- Query shifts that need cash deposit verification (for compliance dashboard)
CREATE INDEX shifts_cash_threshold_idx ON shifts (cash_over_threshold, deposit_verified_at)
    WHERE cash_over_threshold = TRUE AND deposit_verified_at IS NULL;

CREATE TRIGGER shifts_set_updated_at
    BEFORE UPDATE ON shifts
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();
