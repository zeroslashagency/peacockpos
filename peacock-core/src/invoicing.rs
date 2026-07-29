//! Gapless invoice numbering + idempotency.
//!
//! **CGST Rule 46(b):** consecutive series, unique per financial year, ≤16 characters, no gaps.
//!
//! ## Critical invariant
//!
//! The idempotency key must be stored **with** the allocated invoice number. Otherwise
//! a retried submit allocates a second number and gaps the series — the exact failure
//! mode Rule 46(b) forbids.
//!
//! ## Fiscal year: two forms, two jobs
//!
//! India's FY runs April→March, and this module renders it two ways:
//!
//! | function | example | role |
//! |---|---|---|
//! | [`fiscal_year_for`] | `"2026-27"` | human/display form — reports, UI, ledger labels |
//! | [`fiscal_year_code`] | `"2627"` | the form that goes into invoice names |
//!
//! Only the compact code belongs in a name, because the 16-character budget is tight:
//!
//! ```text
//! series (≤4) + "-" (1) + fy code (4) + "-" (1) + counter (6) = 16
//! ```
//!
//! The display form is 7 characters, which would leave a 1-character series budget and
//! reject every realistic prefix. Test the April 1 and March 31 boundaries on both.
//!
//! ## Implementation notes
//!
//! The SQL implementation must be a single `UPDATE … RETURNING` so the increment is
//! atomic and rolls back with the transaction. A failed insert must NOT burn a number.

use crate::error::{Error, Result};
use crate::ids::InvoiceName;
use chrono::{Datelike, NaiveDate};
use uuid::Uuid;

/// CGST Rule 46(b) caps an invoice name at 16 characters.
pub const MAX_INVOICE_NAME_LEN: usize = 16;

/// Port for the row-locked series counter.
///
/// ## SQL implementation contract
///
/// The backing store MUST implement this as a single atomic operation:
///
/// ```sql
/// UPDATE naming_series
///    SET next_number = next_number + 1
///  WHERE series = $1 AND fiscal_year = $2
/// RETURNING next_number - 1
/// ```
///
/// This takes a row lock and ensures the increment rolls back with the surrounding
/// transaction. If the transaction fails (e.g., constraint violation on the invoice
/// insert), the number is NOT burned.
///
/// A gap only appears for a deliberately cancelled invoice, which must carry a
/// logged void reason for the audit trail.
pub trait SeriesAllocator {
    /// Atomically increment and return the next number for the series + fiscal year.
    /// Returns `SeriesNotConfigured` if the series does not exist for this FY.
    fn allocate(&mut self, series: &str, fiscal_year: &str) -> Result<u64>;
}

/// Port for idempotency tracking.
///
/// On replay of the same key, the allocator must return the **original** invoice number.
/// This is the critical link: without it, a retried submit allocates a second number
/// and gaps a legally gapless series.
pub trait IdempotencyStore {
    /// Lookup the invoice number for a previously seen key. Returns None on first sight.
    fn get(&self, key: Uuid) -> Option<InvoiceName>;

    /// Record the allocated invoice number for this key.
    fn record(&mut self, key: Uuid, invoice_name: InvoiceName) -> Result<()>;
}

/// Allocate a gapless invoice number, with idempotency.
///
/// ## Idempotency contract
///
/// On replay of the same `idempotency_key`, this returns the **original** number.
/// The series counter is incremented only once per unique key.
///
/// ## Naming format
///
/// `{series}-{fy_code}-{next:06}`, e.g. `"POS-2627-000001"` (15 characters).
/// `fy_code` is the compact form from [`fiscal_year_code`], not the display form from
/// [`fiscal_year_for`]. The ≤16-character limit is enforced; a name that would exceed it
/// returns [`Error::InvoiceNameTooLong`].
///
/// ## Example
///
/// ```no_run
/// use peacock_core::invoicing::*;
/// use chrono::NaiveDate;
/// use uuid::Uuid;
///
/// # fn example(allocator: &mut impl SeriesAllocator, store: &mut impl IdempotencyStore) -> peacock_core::Result<()> {
/// let key = Uuid::new_v4();
/// let date = NaiveDate::from_ymd_opt(2026, 7, 15).unwrap();
/// let fy = fiscal_year_code(date);
///
/// let name1 = allocate_invoice_number(allocator, store, "URY", &fy, key)?;
/// // Replay returns the same number
/// let name2 = allocate_invoice_number(allocator, store, "URY", &fy, key)?;
/// assert_eq!(name1, name2);
/// # Ok(())
/// # }
/// ```
pub fn allocate_invoice_number(
    allocator: &mut impl SeriesAllocator,
    store: &mut impl IdempotencyStore,
    series: &str,
    fiscal_year: &str,
    idempotency_key: Uuid,
) -> Result<InvoiceName> {
    // Replay returns the original number — this is the critical path.
    if let Some(existing) = store.get(idempotency_key) {
        return Ok(existing);
    }

    // Reject an over-long series BEFORE touching the counter. A rejection after
    // allocating would consume a number and gap a series Rule 46(b) requires to be
    // gapless. COUNTER_PLACEHOLDER stands in for the yet-unallocated number so the
    // probe has the exact length a real name would have.
    const COUNTER_PLACEHOLDER: &str = "000000";
    let probe = format!("{series}-{fiscal_year}-{COUNTER_PLACEHOLDER}");
    if probe.chars().count() > MAX_INVOICE_NAME_LEN {
        return Err(Error::InvoiceNameTooLong {
            name: probe,
            limit: MAX_INVOICE_NAME_LEN,
        });
    }

    // Allocate the next number atomically
    let next = allocator.allocate(series, fiscal_year)?;

    // The probe only bounds a ≤6-digit counter; a series that outgrows 999_999 widens
    // the name past the cap, so re-check the real thing.
    let formatted = format!("{series}-{fiscal_year}-{next:06}");
    if formatted.chars().count() > MAX_INVOICE_NAME_LEN {
        return Err(Error::InvoiceNameTooLong {
            name: formatted,
            limit: MAX_INVOICE_NAME_LEN,
        });
    }

    let invoice_name = InvoiceName::new(formatted);

    // Record the key→number mapping before returning
    store.record(idempotency_key, invoice_name.clone())?;

    Ok(invoice_name)
}

/// Fiscal year for a given date, in **display** form. India's FY runs April→March.
///
/// This is the human-facing label (reports, UI, ledger headings). It is 7 characters, so it
/// does not fit an invoice name — use [`fiscal_year_code`] for that.
///
/// ## Examples
///
/// ```
/// use peacock_core::invoicing::fiscal_year_for;
/// use chrono::NaiveDate;
///
/// assert_eq!(
///     fiscal_year_for(NaiveDate::from_ymd_opt(2026, 3, 31).unwrap()),
///     "2025-26"
/// );
/// assert_eq!(
///     fiscal_year_for(NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()),
///     "2026-27"
/// );
/// assert_eq!(
///     fiscal_year_for(NaiveDate::from_ymd_opt(2026, 7, 15).unwrap()),
///     "2026-27"
/// );
/// ```
pub fn fiscal_year_for(date: NaiveDate) -> String {
    let year = date.year();
    let month = date.month();

    if month >= 4 {
        // April onwards: this year to next year
        format!("{}-{:02}", year, (year + 1) % 100)
    } else {
        // Jan-Mar: previous year to this year
        format!("{}-{:02}", year - 1, year % 100)
    }
}

/// Fiscal year for a given date, in **compact code** form: the two 2-digit year halves
/// concatenated, e.g. `"2627"` for FY 2026-27.
///
/// This is the form that goes into an invoice name. It spends 4 of the 16 characters
/// Rule 46(b) allows, versus 7 for [`fiscal_year_for`], which leaves room for a real
/// series prefix (up to 4 characters) alongside the 6-digit counter.
///
/// Same April→March boundary as [`fiscal_year_for`].
///
/// ## Examples
///
/// ```
/// use peacock_core::invoicing::fiscal_year_code;
/// use chrono::NaiveDate;
///
/// assert_eq!(
///     fiscal_year_code(NaiveDate::from_ymd_opt(2026, 3, 31).unwrap()),
///     "2526"
/// );
/// assert_eq!(
///     fiscal_year_code(NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()),
///     "2627"
/// );
/// assert_eq!(
///     fiscal_year_code(NaiveDate::from_ymd_opt(2026, 7, 15).unwrap()),
///     "2627"
/// );
/// ```
pub fn fiscal_year_code(date: NaiveDate) -> String {
    let year = date.year();

    // Same April→March split as fiscal_year_for; only the rendering differs.
    let start_year = if date.month() >= 4 { year } else { year - 1 };

    format!("{:02}{:02}", start_year % 100, (start_year + 1) % 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // -------------------------------------------------------------------------
    // In-memory fakes for testing
    // -------------------------------------------------------------------------

    struct FakeAllocator {
        counters: HashMap<(String, String), u64>,
        /// Simulates a rollback: the increment is recorded but not persisted
        pending_increment: Option<(String, String)>,
    }

    impl FakeAllocator {
        fn new() -> Self {
            FakeAllocator {
                counters: HashMap::new(),
                pending_increment: None,
            }
        }

        fn seed(&mut self, series: &str, fy: &str, start: u64) {
            self.counters
                .insert((series.to_owned(), fy.to_owned()), start);
        }

        /// Current counter value without consuming it.
        fn peek(&self, series: &str, fy: &str) -> Option<u64> {
            self.counters
                .get(&(series.to_owned(), fy.to_owned()))
                .copied()
        }

        /// Simulate a rollback: the next allocation after this will re-use the number
        fn rollback(&mut self) {
            if let Some((series, fy)) = self.pending_increment.take() {
                if let Some(counter) = self.counters.get_mut(&(series, fy)) {
                    *counter -= 1;
                }
            }
        }
    }

    impl SeriesAllocator for FakeAllocator {
        fn allocate(&mut self, series: &str, fiscal_year: &str) -> Result<u64> {
            let key = (series.to_owned(), fiscal_year.to_owned());
            let counter = self
                .counters
                .entry(key.clone())
                .or_insert(1);

            let next = *counter;
            *counter += 1;
            self.pending_increment = Some(key);
            Ok(next)
        }
    }

    struct FakeIdempotencyStore {
        store: HashMap<Uuid, InvoiceName>,
    }

    impl FakeIdempotencyStore {
        fn new() -> Self {
            FakeIdempotencyStore {
                store: HashMap::new(),
            }
        }
    }

    impl IdempotencyStore for FakeIdempotencyStore {
        fn get(&self, key: Uuid) -> Option<InvoiceName> {
            self.store.get(&key).cloned()
        }

        fn record(&mut self, key: Uuid, invoice_name: InvoiceName) -> Result<()> {
            self.store.insert(key, invoice_name);
            Ok(())
        }
    }

    // -------------------------------------------------------------------------
    // Tests
    // -------------------------------------------------------------------------

    #[test]
    fn fiscal_year_april_boundary() {
        assert_eq!(
            fiscal_year_for(NaiveDate::from_ymd_opt(2026, 3, 31).unwrap()),
            "2025-26"
        );
        assert_eq!(
            fiscal_year_for(NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()),
            "2026-27"
        );
    }

    #[test]
    fn fiscal_year_mid_year() {
        assert_eq!(
            fiscal_year_for(NaiveDate::from_ymd_opt(2026, 7, 15).unwrap()),
            "2026-27"
        );
        assert_eq!(
            fiscal_year_for(NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()),
            "2026-27"
        );
    }

    #[test]
    fn fiscal_year_january_march() {
        assert_eq!(
            fiscal_year_for(NaiveDate::from_ymd_opt(2027, 1, 15).unwrap()),
            "2026-27"
        );
        assert_eq!(
            fiscal_year_for(NaiveDate::from_ymd_opt(2027, 3, 31).unwrap()),
            "2026-27"
        );
    }

    #[test]
    fn sequential_allocation_no_gaps() {
        let mut allocator = FakeAllocator::new();
        allocator.seed("P", "2627", 1);
        let mut store = FakeIdempotencyStore::new();

        let mut names = vec![];
        for _ in 0..5 {
            let key = Uuid::new_v4();
            let name = allocate_invoice_number(
                &mut allocator,
                &mut store,
                "P",
                "2627",
                key,
            )
            .unwrap();
            names.push(name);
        }

        assert_eq!(names[0].as_str(), "P-2627-000001");
        assert_eq!(names[1].as_str(), "P-2627-000002");
        assert_eq!(names[2].as_str(), "P-2627-000003");
        assert_eq!(names[3].as_str(), "P-2627-000004");
        assert_eq!(names[4].as_str(), "P-2627-000005");
    }

    #[test]
    fn idempotency_replays_same_number() {
        let mut allocator = FakeAllocator::new();
        allocator.seed("P", "2627", 1);
        let mut store = FakeIdempotencyStore::new();

        let key = Uuid::new_v4();

        // First call allocates
        let name1 = allocate_invoice_number(
            &mut allocator,
            &mut store,
            "P",
            "2627",
            key,
        )
        .unwrap();

        // Replay with same key returns the same number
        let name2 = allocate_invoice_number(
            &mut allocator,
            &mut store,
            "P",
            "2627",
            key,
        )
        .unwrap();

        assert_eq!(name1, name2);
        assert_eq!(name1.as_str(), "P-2627-000001");

        // The allocator did NOT increment a second time
        let next_key = Uuid::new_v4();
        let name3 = allocate_invoice_number(
            &mut allocator,
            &mut store,
            "P",
            "2627",
            next_key,
        )
        .unwrap();
        assert_eq!(name3.as_str(), "P-2627-000002");
    }

    #[test]
    fn different_keys_get_different_numbers() {
        let mut allocator = FakeAllocator::new();
        allocator.seed("P", "2627", 1);
        let mut store = FakeIdempotencyStore::new();

        let key1 = Uuid::new_v4();
        let key2 = Uuid::new_v4();

        let name1 = allocate_invoice_number(
            &mut allocator,
            &mut store,
            "P",
            "2627",
            key1,
        )
        .unwrap();
        let name2 = allocate_invoice_number(
            &mut allocator,
            &mut store,
            "P",
            "2627",
            key2,
        )
        .unwrap();

        assert_ne!(name1, name2);
        assert_eq!(name1.as_str(), "P-2627-000001");
        assert_eq!(name2.as_str(), "P-2627-000002");
    }

    #[test]
    fn rolled_back_allocation_does_not_burn_number() {
        let mut allocator = FakeAllocator::new();
        allocator.seed("P", "2627", 1);
        let mut store = FakeIdempotencyStore::new();

        let key1 = Uuid::new_v4();

        // Allocate, then simulate a rollback (e.g., transaction failed)
        let _name1 = allocate_invoice_number(
            &mut allocator,
            &mut store,
            "P",
            "2627",
            key1,
        )
        .unwrap();
        allocator.rollback();

        // Next allocation re-uses the number
        let key2 = Uuid::new_v4();
        let name2 = allocate_invoice_number(
            &mut allocator,
            &mut store,
            "P",
            "2627",
            key2,
        )
        .unwrap();
        assert_eq!(name2.as_str(), "P-2627-000001");
    }

    #[test]
    fn sixteen_character_limit_enforced() {
        let mut allocator = FakeAllocator::new();
        allocator.seed("TOOLONG", "2627", 1);
        let mut store = FakeIdempotencyStore::new();

        let key = Uuid::new_v4();

        // "TOOLONG-2627-000001" = 19 chars, exceeds 16
        let result =
            allocate_invoice_number(&mut allocator, &mut store, "TOOLONG", "2627", key);

        match result {
            Err(Error::InvoiceNameTooLong { name, limit }) => {
                assert_eq!(name, "TOOLONG-2627-000000");
                assert_eq!(limit, 16);
            }
            other => panic!("expected InvoiceNameTooLong, got {other:?}"),
        }
    }

    #[test]
    fn over_long_series_does_not_burn_a_number() {
        // The gapless guarantee: a rejected attempt must leave the counter untouched,
        // otherwise a misconfigured series silently punches holes in a legal series.
        let mut allocator = FakeAllocator::new();
        allocator.seed("LONGER", "2627", 7);
        let mut store = FakeIdempotencyStore::new();

        let result = allocate_invoice_number(
            &mut allocator,
            &mut store,
            "LONGER",
            "2627",
            Uuid::new_v4(),
        );
        assert!(matches!(result, Err(Error::InvoiceNameTooLong { .. })));

        assert_eq!(
            allocator.peek("LONGER", "2627"),
            Some(7),
            "counter advanced despite a rejected allocation"
        );
    }

    #[test]
    fn sixteen_character_limit_allows_valid() {
        let mut allocator = FakeAllocator::new();
        allocator.seed("URY", "2627", 1);
        let mut store = FakeIdempotencyStore::new();

        let key = Uuid::new_v4();

        // "URY-2627-000001" = 15 chars, inside the Rule 46(b) cap.
        let name =
            allocate_invoice_number(&mut allocator, &mut store, "URY", "2627", key).unwrap();

        assert_eq!(name.as_str(), "URY-2627-000001");
        assert!(name.as_str().len() <= MAX_INVOICE_NAME_LEN);
    }

    #[test]
    fn four_character_series_hits_the_cap_exactly() {
        let mut allocator = FakeAllocator::new();
        allocator.seed("PCOS", "2627", 1);
        let mut store = FakeIdempotencyStore::new();

        // 4 + 1 + 4 + 1 + 6 = 16, the widest series the budget allows.
        let name =
            allocate_invoice_number(&mut allocator, &mut store, "PCOS", "2627", Uuid::new_v4())
                .unwrap();

        assert_eq!(name.as_str(), "PCOS-2627-000001");
        assert_eq!(name.as_str().len(), MAX_INVOICE_NAME_LEN);
    }

    #[test]
    fn counter_overflow_past_six_digits_is_rejected() {
        // The up-front probe assumes a 6-digit counter; a 7-digit one widens the name.
        let mut allocator = FakeAllocator::new();
        allocator.seed("PCOS", "2627", 1_000_000);
        let mut store = FakeIdempotencyStore::new();

        let result =
            allocate_invoice_number(&mut allocator, &mut store, "PCOS", "2627", Uuid::new_v4());

        match result {
            Err(Error::InvoiceNameTooLong { name, limit }) => {
                assert_eq!(name, "PCOS-2627-1000000");
                assert_eq!(limit, 16);
            }
            other => panic!("expected InvoiceNameTooLong, got {other:?}"),
        }
    }

    #[test]
    fn short_series_fits_in_sixteen_chars() {
        let mut allocator = FakeAllocator::new();
        allocator.seed("POS", "2627", 1);
        let mut store = FakeIdempotencyStore::new();

        let key = Uuid::new_v4();

        // "POS-2627-000001" = 15 chars
        let name =
            allocate_invoice_number(&mut allocator, &mut store, "POS", "2627", key).unwrap();

        assert_eq!(name.as_str(), "POS-2627-000001");
        assert_eq!(name.as_str().len(), 15);
    }

    #[test]
    fn fiscal_year_code_april_boundary() {
        assert_eq!(
            fiscal_year_code(NaiveDate::from_ymd_opt(2026, 3, 31).unwrap()),
            "2526"
        );
        assert_eq!(
            fiscal_year_code(NaiveDate::from_ymd_opt(2026, 4, 1).unwrap()),
            "2627"
        );
    }

    #[test]
    fn fiscal_year_code_mid_year_and_january_march() {
        assert_eq!(
            fiscal_year_code(NaiveDate::from_ymd_opt(2026, 7, 15).unwrap()),
            "2627"
        );
        assert_eq!(
            fiscal_year_code(NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()),
            "2627"
        );
        assert_eq!(
            fiscal_year_code(NaiveDate::from_ymd_opt(2027, 1, 15).unwrap()),
            "2627"
        );
        assert_eq!(
            fiscal_year_code(NaiveDate::from_ymd_opt(2027, 3, 31).unwrap()),
            "2627"
        );
    }

    #[test]
    fn fiscal_year_code_spans_century_rollover() {
        // 1999-2000 renders as "9900", not "99100": both halves are 2-digit.
        assert_eq!(
            fiscal_year_code(NaiveDate::from_ymd_opt(1999, 6, 1).unwrap()),
            "9900"
        );
        assert_eq!(
            fiscal_year_code(NaiveDate::from_ymd_opt(2000, 1, 1).unwrap()),
            "9900"
        );
    }

    #[test]
    fn fiscal_year_code_composes_into_a_legal_invoice_name() {
        // The regression guard. The display form ("2026-27") pushed every 2+ character
        // series past 16 and nothing composed the two functions, so it went unnoticed.
        let date = NaiveDate::from_ymd_opt(2026, 7, 15).unwrap();
        let fy = fiscal_year_code(date);

        let cases = [
            ("P", "P-2627-000001"),
            ("PC", "PC-2627-000001"),
            ("POS", "POS-2627-000001"),
            ("URY", "URY-2627-000001"),
        ];

        for (series, expected) in cases {
            let mut allocator = FakeAllocator::new();
            allocator.seed(series, &fy, 1);
            let mut store = FakeIdempotencyStore::new();

            let name =
                allocate_invoice_number(&mut allocator, &mut store, series, &fy, Uuid::new_v4())
                    .unwrap_or_else(|e| panic!("series {series:?} must allocate, got {e:?}"));

            assert_eq!(name.as_str(), expected);
            assert!(
                name.as_str().len() <= MAX_INVOICE_NAME_LEN,
                "{} is {} chars, over the Rule 46(b) cap",
                name.as_str(),
                name.as_str().len()
            );
        }
    }

    #[test]
    fn simulated_concurrent_allocation_no_duplicates() {
        // Simulate 10 concurrent requests by allocating with unique keys
        let mut allocator = FakeAllocator::new();
        allocator.seed("P", "2627", 1);
        let mut store = FakeIdempotencyStore::new();

        let mut names = vec![];
        for _ in 0..10 {
            let key = Uuid::new_v4();
            let name = allocate_invoice_number(
                &mut allocator,
                &mut store,
                "P",
                "2627",
                key,
            )
            .unwrap();
            names.push(name);
        }

        // All names are unique
        let unique_count = names.iter().collect::<std::collections::HashSet<_>>().len();
        assert_eq!(unique_count, 10);

        // And sequential
        for (i, name) in names.iter().enumerate() {
            let expected = format!("P-2627-{:06}", i + 1);
            assert_eq!(name.as_str(), expected);
        }
    }

    #[test]
    fn idempotency_prevents_duplicate_on_retry() {
        let mut allocator = FakeAllocator::new();
        allocator.seed("P", "2627", 42);
        let mut store = FakeIdempotencyStore::new();

        let key = Uuid::new_v4();

        // First attempt allocates 42
        let name1 = allocate_invoice_number(
            &mut allocator,
            &mut store,
            "P",
            "2627",
            key,
        )
        .unwrap();
        assert_eq!(name1.as_str(), "P-2627-000042");

        // Retry with same key returns 42 again, does NOT allocate 43
        let name2 = allocate_invoice_number(
            &mut allocator,
            &mut store,
            "P",
            "2627",
            key,
        )
        .unwrap();
        assert_eq!(name2.as_str(), "P-2627-000042");

        // A fresh key gets 43
        let fresh_key = Uuid::new_v4();
        let name3 = allocate_invoice_number(
            &mut allocator,
            &mut store,
            "P",
            "2627",
            fresh_key,
        )
        .unwrap();
        assert_eq!(name3.as_str(), "P-2627-000043");
    }

    #[test]
    fn multiple_replays_return_same_number() {
        let mut allocator = FakeAllocator::new();
        allocator.seed("P", "2627", 1);
        let mut store = FakeIdempotencyStore::new();

        let key = Uuid::new_v4();

        let name1 = allocate_invoice_number(
            &mut allocator,
            &mut store,
            "P",
            "2627",
            key,
        )
        .unwrap();

        // Replay 10 times
        for _ in 0..10 {
            let name = allocate_invoice_number(
                &mut allocator,
                &mut store,
                "P",
                "2627",
                key,
            )
            .unwrap();
            assert_eq!(name, name1);
        }

        // Counter only incremented once
        let next_key = Uuid::new_v4();
        let name2 = allocate_invoice_number(
            &mut allocator,
            &mut store,
            "P",
            "2627",
            next_key,
        )
        .unwrap();
        assert_eq!(name2.as_str(), "P-2627-000002");
    }
}
