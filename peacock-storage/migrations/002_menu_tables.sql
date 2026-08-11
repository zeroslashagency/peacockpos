-- Lane 2C: menu tables.
--
-- Covers the four upstream doctypes that menu resolution touches, plus the two child
-- tables on `URY Restaurant` that make room-wise and order-type-wise selection possible:
--
--   URY Menu         (istable=0) → menus
--   URY Menu Item    (istable=1) → menu_items          child of menus
--   URY Menu Course  (istable=0) → menu_courses
--   Menu for Room    (istable=1) → menu_for_room       child of restaurants
--   Order Type Menu  (istable=1) → order_type_menu     child of restaurants
--
-- The istable split is from GROUND-TRUTH.md §"Root vs child split": `ury_menu_item` is a
-- child table, so it gets a parent FK plus an `idx` ordering column and no standalone
-- identity. `ury_menu_course` is a root doctype (autoname `field:course`), so the course
-- name IS the primary key.
--
-- ---------------------------------------------------------------------------
-- Why `menu_courses.idx` is nullable
-- ---------------------------------------------------------------------------
-- Upstream has NO sequence field on `URY Menu Course` (ury_menu_course.json has exactly
-- one field, `course`), and `getMenuCourses` (api.py:106–108) returns courses unordered.
-- peacock-core/src/menu.rs says so in as many words and leaves the port a choice:
-- derive the sequence or add it to the schema.
--
-- It is added here, deliberately NULLABLE. A NULL `idx` is what makes
-- `MenuResolutionRepo::course_sequences` legitimately omit a course, which is the case
-- `menu.rs::course_with_no_sequence_sorts_by_name_only` covers: items in a course with
-- no sequence sort by name only. A NOT NULL column would make that branch unreachable
-- from real storage and the domain rule would go untested.
--
-- ---------------------------------------------------------------------------
-- Why there is no active-date gating
-- ---------------------------------------------------------------------------
-- `URY Menu` has one validity field, `enabled` (Check, default=1) — no date range. The
-- `at: DateTime<Utc>` parameter on `resolve_menu` is unused for exactly this reason. The
-- column is carried so a future migration has somewhere to hang date ranges, and so an
-- operator can retire a menu, but resolution does NOT filter on it: upstream reads
-- `active_menu` / the child-table mapping and never checks the flag (api.py:40–69). A
-- filter here would silently diverge from the site being replaced.

-- ---------------------------------------------------------------------------
-- menus  (URY Menu)
-- ---------------------------------------------------------------------------
-- `price_list` is read_only upstream ("Price List (Auto created)") — URY creates one
-- Price List per menu. It is a real FK here so a menu cannot point at a list that was
-- deleted; ON DELETE SET NULL because losing the list must not delete the menu.

CREATE TABLE menus (
    name       TEXT PRIMARY KEY,
    branch     TEXT        NOT NULL,
    enabled    BOOLEAN     NOT NULL DEFAULT TRUE,
    price_list TEXT
        REFERENCES price_lists (name) ON UPDATE CASCADE ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ,
    CONSTRAINT menus_name_not_blank CHECK (length(btrim(name)) > 0),
    CONSTRAINT menus_branch_not_blank CHECK (length(btrim(branch)) > 0)
);

-- `branch` is reqd upstream and every resolution path is branch-scoped.
CREATE INDEX menus_branch_idx ON menus (branch) WHERE deleted_at IS NULL;
CREATE INDEX menus_enabled_idx ON menus (branch) WHERE enabled AND deleted_at IS NULL;
CREATE INDEX menus_price_list_idx ON menus (price_list);

CREATE TRIGGER menus_set_updated_at
    BEFORE UPDATE ON menus
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- 001_core_tables.sql left `restaurants.active_menu` without a FK and said this lane
-- would add it. This is the default-menu strategy's whole storage: `resolve_menu` with
-- MenuStrategy::Default reads exactly this column (api.py:48, 65, 69).
ALTER TABLE restaurants
    ADD CONSTRAINT restaurants_active_menu_fkey
    FOREIGN KEY (active_menu) REFERENCES menus (name)
    ON UPDATE CASCADE ON DELETE SET NULL;

CREATE INDEX restaurants_active_menu_idx ON restaurants (active_menu)
    WHERE deleted_at IS NULL;

-- ---------------------------------------------------------------------------
-- menu_courses  (URY Menu Course — root doctype, autoname `field:course`)
-- ---------------------------------------------------------------------------
-- The course name is the PK because that is what the child row stores: `URY Menu
-- Item.course` is a Link to `URY Menu Course`, and a Frappe Link holds the docname.
-- MenuCourseName in the domain is a newtype over that same string.

CREATE TABLE menu_courses (
    name       TEXT PRIMARY KEY,
    idx        INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT menu_courses_name_not_blank CHECK (length(btrim(name)) > 0),
    -- A negative sequence sorts before everything including the courses an operator
    -- did sequence, which is never what they meant. Zero is allowed: it is a legitimate
    -- "first" for a 0-based import.
    CONSTRAINT menu_courses_idx_non_negative CHECK (idx IS NULL OR idx >= 0)
);

-- Two courses claiming sequence 3 would make the order of `resolve_menu`'s output
-- depend on the sort's tie-break rather than on configuration. Partial so the
-- unsequenced courses (idx IS NULL) can coexist freely.
CREATE UNIQUE INDEX menu_courses_idx_unique ON menu_courses (idx) WHERE idx IS NOT NULL;
-- course_sequences() reads the whole sequenced set in one query.
CREATE INDEX menu_courses_sequence_idx ON menu_courses (idx) WHERE idx IS NOT NULL;

CREATE TRIGGER menu_courses_set_updated_at
    BEFORE UPDATE ON menu_courses
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ---------------------------------------------------------------------------
-- menu_items  (URY Menu Item — child of menus)
-- ---------------------------------------------------------------------------
-- Fields transcribed from ury_menu_item.json: item, item_name, rate, special_dish,
-- disabled, course_icon, course.
--
--   * rate is NUMERIC(18,6), never float. This is THE selling price for restaurant
--     orders (api.py:79, 87) — not `Item Price`, not `Item.standard_rate`. money.rs is
--     explicit that money is Decimal only; a float here would reintroduce paisa drift.
--     6 dp matches item_prices.rate so a rate copied between the two cannot be
--     truncated in transit.
--   * item_name is `fetch_from: item.item_name` upstream, i.e. a denormalised copy that
--     can go stale. Kept because the child row is what upstream reads, and the
--     repository COALESCEs it against items.name so a blank copy still renders.
--   * PRIMARY KEY (menu, item): one row per item per menu. Upstream allows duplicates
--     (a child table has no such constraint), but two rows for one item mean two rates
--     and the resolved menu would show whichever the sort happened to surface.
--   * idx is the child-table ordering column required by the plan's child-table rule.
--     It is NOT the display order: `resolve_menu` sorts by (course_sequence, item_name)
--     and upstream orders by `item_name asc` (api.py:79). idx preserves the grid order
--     an operator typed, which is the only thing that can reproduce their view.

CREATE TABLE menu_items (
    menu         TEXT           NOT NULL
        REFERENCES menus (name) ON UPDATE CASCADE ON DELETE CASCADE,
    idx          INTEGER        NOT NULL,
    item         TEXT           NOT NULL
        REFERENCES items (code) ON UPDATE CASCADE ON DELETE RESTRICT,
    item_name    TEXT,
    rate         NUMERIC(18, 6) NOT NULL DEFAULT 0,
    special_dish BOOLEAN        NOT NULL DEFAULT FALSE,
    disabled     BOOLEAN        NOT NULL DEFAULT FALSE,
    course       TEXT
        REFERENCES menu_courses (name) ON UPDATE CASCADE ON DELETE SET NULL,
    course_icon  TEXT,
    created_at   TIMESTAMPTZ    NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ    NOT NULL DEFAULT now(),
    PRIMARY KEY (menu, item),
    CONSTRAINT menu_items_idx_positive CHECK (idx > 0),
    -- A negative selling price is not a discount, it is a data-entry accident that
    -- would flow straight into an invoice line.
    CONSTRAINT menu_items_rate_non_negative CHECK (rate >= 0)
);

-- Child rows are ordered within their parent, and two rows cannot claim one position.
CREATE UNIQUE INDEX menu_items_order_idx ON menu_items (menu, idx);
-- `MenuResolutionRepo::menu_items` reads one menu's enabled rows (api.py:76–80).
CREATE INDEX menu_items_enabled_idx ON menu_items (menu) WHERE NOT disabled;
-- `MenuRepo::courses_for_menu` groups by course; the API's course view does too.
CREATE INDEX menu_items_course_idx ON menu_items (menu, course) WHERE NOT disabled;
-- "which menus sell this item" — the reverse lookup, e.g. when an item is retired.
CREATE INDEX menu_items_item_idx ON menu_items (item);

CREATE TRIGGER menu_items_set_updated_at
    BEFORE UPDATE ON menu_items
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ---------------------------------------------------------------------------
-- menu_for_room  (Menu for Room — child of restaurants)
-- ---------------------------------------------------------------------------
-- Strategy 1. Upstream reads it as
--   frappe.db.get_value("Menu for Room", {"parent": restaurant, "room": room}, "menu")
-- (api.py:40–44), so (restaurant, room) is the lookup key and therefore the PK. A
-- second row for the same room would make the resolved menu depend on row order, which
-- is the kind of ambiguity `get_value` hides by returning the first match.

CREATE TABLE menu_for_room (
    restaurant TEXT        NOT NULL
        REFERENCES restaurants (name) ON UPDATE CASCADE ON DELETE CASCADE,
    idx        INTEGER     NOT NULL,
    room       TEXT        NOT NULL
        REFERENCES rooms (name) ON UPDATE CASCADE ON DELETE CASCADE,
    menu       TEXT        NOT NULL
        REFERENCES menus (name) ON UPDATE CASCADE ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (restaurant, room),
    CONSTRAINT menu_for_room_idx_positive CHECK (idx > 0)
);

CREATE UNIQUE INDEX menu_for_room_order_idx ON menu_for_room (restaurant, idx);
-- Resolution arrives with a room and no restaurant when the caller only knows the
-- table it is serving, so the room side needs its own index.
CREATE INDEX menu_for_room_room_idx ON menu_for_room (room);
CREATE INDEX menu_for_room_menu_idx ON menu_for_room (menu);

-- ---------------------------------------------------------------------------
-- order_type_menu  (Order Type Menu — child of restaurants)
-- ---------------------------------------------------------------------------
-- Strategy 2 (api.py:56–60). `order_type` is a Select upstream with options
-- "\nPhone In\nTake Away\nDelivery" — a blank plus three values.
--
-- No CHECK pins that list. A Frappe Select is a UI hint, not a constraint: the options
-- string is edited in the doctype and existing rows are never migrated, so pinning the
-- three values here would reject a fourth order type the moment an operator adds one,
-- and the failure would surface as a 500 on an unrelated request. The domain models
-- `order_type` as a plain String (MenuStrategy::OrderType) for the same reason.
-- Blank is rejected, because a blank order type cannot be what anyone configured.

CREATE TABLE order_type_menu (
    restaurant TEXT        NOT NULL
        REFERENCES restaurants (name) ON UPDATE CASCADE ON DELETE CASCADE,
    idx        INTEGER     NOT NULL,
    order_type TEXT        NOT NULL,
    menu       TEXT        NOT NULL
        REFERENCES menus (name) ON UPDATE CASCADE ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (restaurant, order_type),
    CONSTRAINT order_type_menu_idx_positive CHECK (idx > 0),
    CONSTRAINT order_type_menu_type_not_blank CHECK (length(btrim(order_type)) > 0)
);

CREATE UNIQUE INDEX order_type_menu_order_idx ON order_type_menu (restaurant, idx);
CREATE INDEX order_type_menu_menu_idx ON order_type_menu (menu);
