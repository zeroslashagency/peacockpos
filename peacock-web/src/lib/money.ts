/**
 * Money helpers — paisa-accurate, string end-to-end, never JS Number.
 *
 * Mirrors peacock-core/src/money.rs: Money is Decimal as string, rounded via
 * MidpointAwayFromZero (half-away-from-zero). All arithmetic goes through
 * decimal.js so 0.1 + 0.2 stays 0.3 and no paisa is lost to IEEE-754.
 *
 * Every function takes and returns `string`. The only place a Decimal appears
 * is internally. Callers never touch Number.
 */
import Decimal from "decimal.js";

type Dec = InstanceType<typeof Decimal>;
const ROUND_HALF_UP: number = (Decimal as unknown as { ROUND_HALF_UP: number }).ROUND_HALF_UP;

// Configure Decimal to match Rust's ROUNDING = MidpointAwayFromZero.
(Decimal as unknown as { set: (opts: unknown) => void }).set({ precision: 28, rounding: ROUND_HALF_UP });

export type MoneyString = string;

const PAISA_SCALE = 2;
export const ZERO: MoneyString = "0.00";

// ---------------------------------------------------------------------------
// Parsing & validation
// ---------------------------------------------------------------------------

/**
 * Validate that `value` is a parseable decimal string.
 * Accepts optional sign, digits, optional decimal point.
 */
export function isValidMoney(value: string): boolean {
  if (typeof value !== "string") return false;
  const trimmed = value.trim();
  if (trimmed === "") return false;
  try {
    const d = new Decimal(trimmed);
    return d.isFinite();
  } catch {
    return false;
  }
}

/**
 * Parse a money string into a Decimal.
 * Throws if invalid — caller should guard with isValidMoney or catch.
 * Never goes through Number.
 */
export function parseMoney(value: MoneyString): Dec {
  const trimmed = value.trim();
  if (trimmed === "") throw new Error(`parseMoney: empty string`);
  try {
    const d = new Decimal(trimmed) as Dec;
    if (!d.isFinite()) throw new Error(`non-finite: ${value}`);
    return d;
  } catch (e) {
    throw new Error(`parseMoney: invalid decimal ${JSON.stringify(value)}: ${e}`);
  }
}

/**
 * Ensure a Decimal is rendered with exactly 2 decimal places (paisa).
 * Uses toFixed(2) which respects the rounding mode above.
 */
function toPaisaString(d: Dec): MoneyString {
  // round to 2dp first, then format with exactly 2 places so "540" -> "540.00"
  const rounded = (d as unknown as { toDecimalPlaces: (a: number, b: number) => Dec }).toDecimalPlaces(PAISA_SCALE, ROUND_HALF_UP);
  return rounded.toFixed(PAISA_SCALE);
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/**
 * Format a money string for display in INR.
 * - Rounds to paisa (2dp, half-away-from-zero)
 * - Optionally with currency symbol and grouping.
 *
 * @example formatMoney("1234.5") -> "₹1,234.50"
 * @example formatMoney("1234.5", { symbol: false }) -> "1,234.50"
 */
export function formatMoney(
  value: MoneyString,
  opts: { symbol?: boolean; locale?: string } = {}
): string {
  const { symbol = true, locale = "en-IN" } = opts;
  const d = parseMoney(value);
  const paisa = toPaisaString(d);
  // Use Intl for grouping, but keep our paisa string as source of truth —
  // Intl.NumberFormat would re-parse via Number, so we group manually via Decimal.
  // Instead, split paisa string and group integer part with en-IN rules via string.
  const [intPart, fracPart] = paisa.split(".");
  const isNegative = intPart.startsWith("-");
  const absInt = isNegative ? intPart.slice(1) : intPart;
  // en-IN grouping: last 3 digits, then groups of 2. Use Intl with string->safe
  // range: integer < 1e15 fits safely in Number for grouping only.
  // For larger values fall back to manual grouping (still correct).
  let grouped: string;
  try {
    // Only for grouping, value is integer part which is < 2^53 in practice.
    // If it exceeds safe integer, we do manual.
    const intNum = Number(absInt); // grouping only; paisa kept as string
    if (Number.isSafeInteger(intNum)) {
      grouped = new Intl.NumberFormat(locale, { useGrouping: true }).format(intNum);
    } else {
      grouped = groupIndian(absInt);
    }
  } catch {
    grouped = groupIndian(absInt);
  }
  const sign = isNegative ? "-" : "";
  const formatted = `${sign}${grouped}.${fracPart}`;
  return symbol ? `₹${formatted}` : formatted;
}

function groupIndian(digits: string): string {
  if (digits.length <= 3) return digits;
  let result = digits.slice(-3);
  let idx = digits.length - 3;
  while (idx > 0) {
    const start = Math.max(0, idx - 2);
    result = digits.slice(start, idx) + "," + result;
    idx = start;
  }
  return result;
}

/**
 * Compact format without grouping, exactly 2dp. For inputs/serialization.
 */
export function normalizeMoney(value: MoneyString): MoneyString {
  return toPaisaString(parseMoney(value));
}

// ---------------------------------------------------------------------------
// Arithmetic — all return MoneyString (paisa-rounded where appropriate)
// ---------------------------------------------------------------------------

export function addMoney(a: MoneyString, b: MoneyString): MoneyString {
  return toPaisaString(parseMoney(a).plus(parseMoney(b)) as Dec);
}

export function subMoney(a: MoneyString, b: MoneyString): MoneyString {
  return toPaisaString(parseMoney(a).minus(parseMoney(b)) as Dec);
}

/**
 * Multiply money by a decimal factor (quantity or rate).
 * Factor is also a string to avoid Number.
 * Result is paisa-rounded.
 */
export function mulMoney(money: MoneyString, factor: MoneyString | string): MoneyString {
  const a: unknown = parseMoney(money);
  // decimal.js supports both mul and times; use any to satisfy both typings
  const m = a as { mul: (b: unknown) => Dec; times: (b: unknown) => Dec };
  const fn = (m.mul ?? m.times).bind(m);
  return toPaisaString(fn(parseMoney(String(factor))) as Dec);
}

/**
 * Sum an array of money strings, paisa-rounded once at the end.
 * Mirrors peacock-core's `Sum for Money`: sum then round.
 */
export function sumMoney(values: MoneyString[]): MoneyString {
  if (values.length === 0) return ZERO;
  const total = values.reduce((acc, v) => acc.plus(parseMoney(v)) as Dec, new Decimal(0) as Dec);
  return toPaisaString(total);
}

/**
 * Compare two money strings.
 * @returns -1 if a < b, 0 if equal, 1 if a > b
 */
export function cmpMoney(a: MoneyString, b: MoneyString): -1 | 0 | 1 {
  const da = parseMoney(a);
  const db = parseMoney(b);
  if (da.lt(db)) return -1;
  if (da.gt(db)) return 1;
  return 0;
}

export function isZero(value: MoneyString): boolean {
  return parseMoney(value).isZero();
}

export function isNegative(value: MoneyString): boolean {
  return parseMoney(value).isNegative();
}

/**
 * Round to nearest rupee (integer), half-away-from-zero.
 * Used for `rounded_total` / `round_off` at invoice level.
 */
export function toRupee(value: MoneyString): MoneyString {
  const d = parseMoney(value);
  // Decimal toDecimalPlaces(0) with HALF_UP
  return (d as unknown as { toDecimalPlaces: (a: number, b: number) => Dec }).toDecimalPlaces(0, ROUND_HALF_UP).toFixed(0);
}

/**
 * Round to paisa (2dp) — explicit alias for normalizeMoney.
 */
export function toPaisa(value: MoneyString): MoneyString {
  return toPaisaString(parseMoney(value));
}

/**
 * Compute round-off residual: rounded_total - grand_total
 * Both inputs are MoneyString; result is MoneyString with 2dp.
 */
export function roundOff(grandTotal: MoneyString): { roundedTotal: MoneyString; roundOff: MoneyString } {
  const grand = parseMoney(grandTotal);
  const rounded = (grand as unknown as { toDecimalPlaces: (a: number, b: number) => Dec }).toDecimalPlaces(0, ROUND_HALF_UP);
  const delta = rounded.minus(grand) as Dec;
  return {
    roundedTotal: rounded.toFixed(0),
    // round_off keeps 2dp (e.g. "0.40" or "-0.40")
    roundOff: (delta as unknown as { toDecimalPlaces: (a: number, b: number) => Dec }).toDecimalPlaces(2, ROUND_HALF_UP).toFixed(2),
  };
}
