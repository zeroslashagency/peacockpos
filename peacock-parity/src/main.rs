//! Parity harness — validates Rust money arithmetic against Python oracle.
//!
//! Reads JSON fixtures, runs them through peacock-core, runs them through the
//! Python reference implementation, diffs the results to the paisa. Exit non-zero
//! on any diff.

use anyhow::{Context, Result};
use colored::Colorize;
use peacock_core::cogs::{cogs_for_item_with_bundles, CogsResult, MAX_LEVEL};
use peacock_core::ids::*;
use peacock_core::money::Money;
use peacock_core::ports::{
    Bom, BomLine, BomRepo, PriceRepo, ProductBundle, ProductBundleLine, ProductBundleRepo,
};
use peacock_core::tax::{compute_totals, DiscountBasis, InvoiceLine, InvoiceTotals, SupplyType};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;

// ============================================================================
// Fixture schema
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Fixture {
    Tax(TaxFixture),
    Cogs(CogsFixture),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaxFixture {
    name: String,
    lines: Vec<LineFixture>,
    discount: String,
    tax_rate: String,
    supply_type: String,
    discount_basis: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LineFixture {
    quantity: String,
    rate: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CogsFixture {
    name: String,
    item: String,
    qty: String,
    buying_price_list: String,
    boms: HashMap<String, BomFixture>,
    /// Product Bundles keyed by `new_item_code`. Omitted by the pre-bundle
    /// fixtures, which then exercise only the BOM and plain buckets.
    #[serde(default)]
    bundles: HashMap<String, Vec<BundleLineFixture>>,
    prices: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BundleLineFixture {
    item_code: String,
    qty: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BomFixture {
    quantity: String,
    items: Vec<BomLineFixture>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BomLineFixture {
    item_code: String,
    qty: String,
}

// ============================================================================
// Python results schema
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PythonResult {
    Tax(PythonTaxResult),
    Cogs(PythonCogsResult),
}

#[derive(Debug, Deserialize)]
struct PythonTaxResult {
    #[allow(dead_code)]
    name: String,
    net_total: String,
    discount: String,
    taxable_value: String,
    cgst: String,
    sgst: String,
    igst: String,
    total_tax: String,
    grand_total: String,
    rounded_total: String,
    round_off: String,
}

#[derive(Debug, Deserialize)]
struct PythonCogsResult {
    #[allow(dead_code)]
    name: String,
    cost: String,
    unset_item_prices: Vec<String>,
    unset_bundle_items: Vec<String>,
    unset_bom_items: Vec<String>,
}

// ============================================================================
// In-memory fake repos
// ============================================================================

struct FakeBomRepo {
    boms: HashMap<ItemCode, Bom>,
}

impl FakeBomRepo {
    fn from_fixture(boms_map: &HashMap<String, BomFixture>) -> Result<Self> {
        let mut boms = HashMap::new();
        for (item_code, bom_fix) in boms_map {
            let quantity = Decimal::from_str(&bom_fix.quantity)
                .context(format!("parsing bom quantity for {}", item_code))?;
            let items: Result<Vec<_>> = bom_fix
                .items
                .iter()
                .map(|ln| {
                    Ok(BomLine {
                        item_code: ItemCode::from(ln.item_code.as_str()),
                        qty: Decimal::from_str(&ln.qty)
                            .context(format!("parsing bom line qty for {}", ln.item_code))?,
                    })
                })
                .collect();
            boms.insert(
                ItemCode::from(item_code.as_str()),
                Bom {
                    name: BomName::from(format!("BOM-{}", item_code).as_str()),
                    quantity,
                    items: items?,
                },
            );
        }
        Ok(FakeBomRepo { boms })
    }
}

impl BomRepo for FakeBomRepo {
    fn find_for_item(&self, item: &ItemCode) -> peacock_core::Result<Option<Bom>> {
        Ok(self.boms.get(item).cloned())
    }
}

struct FakePriceRepo {
    prices: HashMap<ItemCode, Money>,
}

impl FakePriceRepo {
    fn from_fixture(prices_map: &HashMap<String, String>) -> Result<Self> {
        let mut prices = HashMap::new();
        for (item_code, price_str) in prices_map {
            let dec = Decimal::from_str(price_str)
                .context(format!("parsing price for {}", item_code))?;
            prices.insert(ItemCode::from(item_code.as_str()), Money::new(dec));
        }
        Ok(FakePriceRepo { prices })
    }
}

impl PriceRepo for FakePriceRepo {
    fn item_price(
        &self,
        item: &ItemCode,
        _price_list: &PriceListName,
    ) -> peacock_core::Result<Option<Money>> {
        Ok(self.prices.get(item).copied())
    }
}

struct FakeBundleRepo {
    bundles: HashMap<ItemCode, ProductBundle>,
}

impl FakeBundleRepo {
    fn from_fixture(
        bundles_map: &HashMap<String, Vec<BundleLineFixture>>,
    ) -> Result<Self> {
        let mut bundles = HashMap::new();
        for (new_item_code, lines) in bundles_map {
            let items: Result<Vec<_>> = lines
                .iter()
                .map(|ln| {
                    Ok(ProductBundleLine {
                        item_code: ItemCode::from(ln.item_code.as_str()),
                        qty: Decimal::from_str(&ln.qty)
                            .context(format!("parsing bundle line qty for {}", ln.item_code))?,
                    })
                })
                .collect();
            bundles.insert(
                ItemCode::from(new_item_code.as_str()),
                ProductBundle {
                    new_item_code: ItemCode::from(new_item_code.as_str()),
                    items: items?,
                },
            );
        }
        Ok(FakeBundleRepo { bundles })
    }
}

impl ProductBundleRepo for FakeBundleRepo {
    fn find_by_new_item_code(
        &self,
        item: &ItemCode,
    ) -> peacock_core::Result<Option<ProductBundle>> {
        Ok(self.bundles.get(item).cloned())
    }
}

// ============================================================================
// Rust computation
// ============================================================================

fn run_tax_fixture_rust(fx: &TaxFixture) -> Result<InvoiceTotals> {
    let lines: Result<Vec<_>> = fx
        .lines
        .iter()
        .map(|ln| {
            Ok(InvoiceLine {
                item_name: "Test Item".to_owned(),
                quantity: Decimal::from_str(&ln.quantity)?,
                rate: Money::new(Decimal::from_str(&ln.rate)?),
                hsn_sac: None,
            })
        })
        .collect();
    let lines = lines?;

    let discount = Money::new(Decimal::from_str(&fx.discount)?);
    let tax_rate = Decimal::from_str(&fx.tax_rate)?;

    let supply_type = match fx.supply_type.as_str() {
        "intrastate" => SupplyType::Intrastate,
        "interstate" => SupplyType::Interstate,
        _ => anyhow::bail!("unknown supply_type: {}", fx.supply_type),
    };

    let discount_basis = match fx.discount_basis.as_str() {
        "net_total" => DiscountBasis::NetTotal,
        "grand_total" => DiscountBasis::GrandTotal,
        _ => anyhow::bail!("unknown discount_basis: {}", fx.discount_basis),
    };

    let totals = compute_totals(&lines, discount, tax_rate, supply_type, discount_basis)?;
    Ok(totals)
}

fn run_cogs_fixture_rust(fx: &CogsFixture) -> Result<CogsResult> {
    let item = ItemCode::from(fx.item.as_str());
    let qty = Decimal::from_str(&fx.qty)?;
    let buying_price_list = PriceListName::from(fx.buying_price_list.as_str());

    let boms = FakeBomRepo::from_fixture(&fx.boms)?;
    let bundles = FakeBundleRepo::from_fixture(&fx.bundles)?;
    let prices = FakePriceRepo::from_fixture(&fx.prices)?;

    // Always the bundle-aware entry point, so every fixture also exercises the
    // three-way precedence. With an empty `bundles` map it reduces to BOM -> plain.
    let result = cogs_for_item_with_bundles(
        &item,
        qty,
        &buying_price_list,
        &bundles,
        &boms,
        &prices,
    )?;
    Ok(result)
}

// ============================================================================
// Python invocation
// ============================================================================

fn run_python_reference(fixtures: &[Fixture]) -> Result<Vec<PythonResult>> {
    let script_path = PathBuf::from("scripts/parity_reference.py");
    if !script_path.exists() {
        anyhow::bail!(
            "Python reference script not found at {}",
            script_path.display()
        );
    }

    let fixture_json = serde_json::to_string(fixtures)?;

    let mut child = Command::new("python3")
        .arg(&script_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning python3")?;

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(fixture_json.as_bytes())?;

    let output = child.wait_with_output()?;

    if !output.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        anyhow::bail!("Python reference failed with status {}", output.status);
    }

    let results: Vec<PythonResult> = serde_json::from_slice(&output.stdout)
        .context("parsing Python output")?;

    Ok(results)
}

// ============================================================================
// Diffing
// ============================================================================

#[derive(Debug)]
struct Diff {
    fixture: String,
    field: String,
    python: String,
    rust: String,
    delta: String,
}

fn diff_tax(name: &str, py: &PythonTaxResult, rust: &InvoiceTotals) -> Vec<Diff> {
    let mut diffs = Vec::new();

    macro_rules! check {
        ($field:expr, $py_val:expr, $rust_val:expr) => {
            let py_dec = Decimal::from_str($py_val).unwrap();
            let rust_dec = $rust_val.inner();
            if py_dec != rust_dec {
                diffs.push(Diff {
                    fixture: name.to_owned(),
                    field: $field.to_owned(),
                    python: py_dec.to_string(),
                    rust: rust_dec.to_string(),
                    delta: (rust_dec - py_dec).to_string(),
                });
            }
        };
    }

    check!("net_total", &py.net_total, rust.net_total);
    check!("discount", &py.discount, rust.discount);
    check!("taxable_value", &py.taxable_value, rust.taxable_value);
    check!("cgst", &py.cgst, rust.tax.cgst);
    check!("sgst", &py.sgst, rust.tax.sgst);
    check!("igst", &py.igst, rust.tax.igst);
    check!("total_tax", &py.total_tax, rust.tax.total_tax);
    check!("grand_total", &py.grand_total, rust.grand_total);
    check!("rounded_total", &py.rounded_total, rust.rounded_total);
    check!("round_off", &py.round_off, rust.round_off);

    diffs
}

fn diff_cogs(name: &str, py: &PythonCogsResult, rust: &CogsResult) -> Vec<Diff> {
    let mut diffs = Vec::new();

    let py_cost = Decimal::from_str(&py.cost).unwrap();
    let rust_cost = rust.cost.inner();

    if py_cost != rust_cost {
        diffs.push(Diff {
            fixture: name.to_owned(),
            field: "cost".to_owned(),
            python: py_cost.to_string(),
            rust: rust_cost.to_string(),
            delta: (rust_cost - py_cost).to_string(),
        });
    }

    // All three unset lists are diffed separately. Comparing only their union would
    // let a mislabelled data gap through, and the label is what tells the operator
    // where to go fix the price (ury_daily_p_and_l.py:262-264).
    let mut check_list = |field: &str, py_list: &[String], rust_list: &[ItemCode]| {
        let mut py_unset = py_list.to_vec();
        let mut rust_unset: Vec<String> =
            rust_list.iter().map(|ic| ic.as_str().to_owned()).collect();
        py_unset.sort();
        rust_unset.sort();

        if py_unset != rust_unset {
            diffs.push(Diff {
                fixture: name.to_owned(),
                field: field.to_owned(),
                python: format!("{:?}", py_unset),
                rust: format!("{:?}", rust_unset),
                delta: "(list mismatch)".to_owned(),
            });
        }
    };

    check_list("unset_item_prices", &py.unset_item_prices, &rust.unset_item_prices);
    check_list(
        "unset_bundle_items",
        &py.unset_bundle_items,
        &rust.unset_bundle_items,
    );
    check_list("unset_bom_items", &py.unset_bom_items, &rust.unset_bom_items);

    diffs
}

// ============================================================================
// Main harness
// ============================================================================

fn load_fixtures(fixture_dir: &Path) -> Result<Vec<Fixture>> {
    let mut fixtures = Vec::new();

    for entry in fs::read_dir(fixture_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            let content = fs::read_to_string(&path)
                .context(format!("reading {}", path.display()))?;
            let fx: Fixture = serde_json::from_str(&content)
                .context(format!("parsing {}", path.display()))?;
            fixtures.push(fx);
        }
    }

    fixtures.sort_by_key(|fx| match fx {
        Fixture::Tax(t) => t.name.clone(),
        Fixture::Cogs(c) => c.name.clone(),
    });

    Ok(fixtures)
}

fn main() -> Result<()> {
    println!("{}", "═══ Peacock Parity Harness ═══".bold());
    println!();
    println!("Validating Rust implementations against Python oracle.");
    println!("Python reference: scripts/parity_reference.py");
    println!("Rust implementations: peacock-core/src/{{tax.rs, cogs.rs}}");
    println!();

    // Load fixtures
    let fixture_dir = PathBuf::from("peacock-parity/fixtures");
    if !fixture_dir.exists() {
        anyhow::bail!("Fixture directory not found: {}", fixture_dir.display());
    }

    println!("Loading fixtures from {}...", fixture_dir.display());
    let fixtures = load_fixtures(&fixture_dir)?;
    println!("Loaded {} fixtures.", fixtures.len());
    println!();

    if fixtures.is_empty() {
        println!("{}", "⚠ No fixtures found. Nothing to verify.".yellow());
        return Ok(());
    }

    // Run Python reference
    println!("Running Python reference...");
    let py_results = run_python_reference(&fixtures)?;
    println!("{}", "✓ Python complete".green());
    println!();

    // Run Rust implementations and diff
    println!("Running Rust implementations and diffing...");
    let mut all_diffs = Vec::new();

    for (fx, py_res) in fixtures.iter().zip(py_results.iter()) {
        match (fx, py_res) {
            (Fixture::Tax(tax_fx), PythonResult::Tax(py_tax)) => {
                let rust_totals = run_tax_fixture_rust(tax_fx)?;
                let diffs = diff_tax(&tax_fx.name, py_tax, &rust_totals);
                all_diffs.extend(diffs);
            }
            (Fixture::Cogs(cogs_fx), PythonResult::Cogs(py_cogs)) => {
                let rust_result = run_cogs_fixture_rust(cogs_fx)?;
                let diffs = diff_cogs(&cogs_fx.name, py_cogs, &rust_result);
                all_diffs.extend(diffs);
            }
            _ => anyhow::bail!("fixture/result kind mismatch"),
        }
    }

    println!();

    if all_diffs.is_empty() {
        println!("{}", "✓ ALL FIXTURES MATCH TO THE PAISA".green().bold());
        println!();
        println!("  Python and Rust agree on:");
        println!("    - Tax calculations (net, taxable, CGST, SGST, IGST, rounding)");
        println!("    - COGS calculations (per-unit normalisation, two-level explosion)");
        println!("    - Product Bundle COGS (bundle > BOM > plain precedence, no extra depth)");
        println!("    - All three unset-item lists, kept separate by label");
        println!();
        println!("  Tested {} fixtures.", fixtures.len());
        println!("  COGS MAX_LEVEL = {} (matches upstream).", MAX_LEVEL);
        println!("  Rounding: peacock_core::money::ROUNDING vs parity_reference.ROUNDING");
        println!("            (half-away-from-zero; NOT yet confirmed against the site).");
        println!();
        Ok(())
    } else {
        println!("{}", "✗ DIFFS FOUND".red().bold());
        println!();
        println!(
            "{:<30} {:<20} {:<20} {:<20} {:<20}",
            "Fixture", "Field", "Python", "Rust", "Delta"
        );
        println!("{}", "─".repeat(110));

        for diff in &all_diffs {
            println!(
                "{:<30} {:<20} {:<20} {:<20} {:<20}",
                diff.fixture.bright_yellow(),
                diff.field,
                diff.python,
                diff.rust,
                diff.delta.red()
            );
        }

        println!();
        println!(
            "{}",
            format!("{} diffs across {} fixtures.", all_diffs.len(), fixtures.len()).red()
        );
        println!();
        anyhow::bail!("Parity check failed");
    }
}
