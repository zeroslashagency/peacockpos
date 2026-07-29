//! # peacock-core
//!
//! URY POS domain logic, ported from Python/Frappe to Rust against verified source.
//!
//! ## Ground rules
//!
//! - Every entity matches the real doctype JSON. See `GROUND-TRUTH.md`.
//! - `URY Order` is a **UI form**, not the order of record. ERPNext's POS Invoice is.
//! - Money is `Decimal`, never `f64`, and crosses the wire as a string.
//! - Storage is behind traits in [`ports`], so all rules are testable without a database.
//!
//! ## Bugs fixed relative to upstream
//!
//! | # | Upstream | Fixed in |
//! |---|---|---|
//! | 1 | `production_items` accumulates across stations (`ury_kot_validation.py:51`) | [`kot`] |
//! | 2 | `posting_date` DATE filtered against datetime bounds (`sub_pos_closing.py:42`) | [`businessday`] |
//! | 3 | `grand_total` vs `rounded_total` revenue split | [`model::PosInvoiceStatus::REVENUE`] |
//! | 4 | `status = "Paid"` vs `IN ("Consolidated","Paid")` | [`model::PosInvoiceStatus::REVENUE`] |
//! | 6 | N+1 `frappe.db.get_value("Item", …)` (`ury_kot_generate.py:154`) | [`ports::ItemRepo::item_groups`] |
//! | 7 | N+1 `frappe.get_doc("Item", …)` (`ury_kot_generate.py:214`) | [`ports::ItemRepo::item_groups`] |

pub mod businessday;
pub mod cogs;
pub mod error;
pub mod ids;
pub mod invoicing;
pub mod kot;
pub mod menu;
pub mod merge;
pub mod model;
pub mod money;
pub mod ports;
pub mod tax;

pub use error::{Error, Result};
pub use money::Money;
