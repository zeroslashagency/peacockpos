/**
 * Typed API client for Peacock POS — generated from docs/API.md
 * and peacock-api/src/routes/* + dto/* shapes.
 *
 * - baseUrl from NEXT_PUBLIC_API_URL (default http://100.72.103.1:8080)
 * - money fields are `string` end-to-end, never number
 * - errors are RFC 7807 Problem JSON (type, title, status, detail, instance, request_id)
 * - supports Idempotency-Key and X-Restaurant headers
 * - no auth (Wave 3 auth-less)
 */

const _rawApiBase =
  (typeof process !== "undefined" && process.env.NEXT_PUBLIC_API_URL) || "";
const _cleanedApiBase = _rawApiBase.replace(/^=+/, "").trim();
// Force same-origin (rewrites) for http:// to avoid https→http mixed-content on Vercel
export const API_BASE_URL = _cleanedApiBase.startsWith("http://") ? "" : _cleanedApiBase;

export function apiBase(): string {
  return API_BASE_URL.replace(/\/$/, "");
}

// ---------------------------------------------------------------------------
// RFC 7807 Problem JSON
// ---------------------------------------------------------------------------
export interface ProblemDetails {
  type: string;
  title: string;
  status: number;
  detail: string;
  instance?: string;
  request_id?: string;
}

export class ApiError extends Error {
  problem: ProblemDetails;
  status: number;
  constructor(problem: ProblemDetails) {
    super(`${problem.title}: ${problem.detail}`);
    this.name = "ApiError";
    this.problem = problem;
    this.status = problem.status;
  }
  isNotFound(): boolean { return this.status === 404; }
  isConflict(): boolean { return this.status === 409; }
  isBadRequest(): boolean { return this.status === 400; }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------
export type MoneyString = string;

export interface RequestOptions {
  /** UUID for idempotency — sent as Idempotency-Key / idempotency-key */
  idempotencyKey?: string;
  /** Restaurant scope — sent as X-Restaurant */
  restaurant?: string;
  /** Extra headers */
  headers?: Record<string, string>;
  /** Fetch signal for cancellation */
  signal?: AbortSignal;
}

function buildHeaders(opts?: RequestOptions, hasBody = true): Headers {
  const h = new Headers();
  if (hasBody) h.set("Content-Type", "application/json");
  h.set("Accept", "application/json, application/problem+json");
  if (opts?.idempotencyKey) {
    h.set("Idempotency-Key", opts.idempotencyKey);
    // also lower-case variant for servers that check lower-case
    h.set("idempotency-key", opts.idempotencyKey);
  }
  if (opts?.restaurant) {
    h.set("X-Restaurant", opts.restaurant);
    h.set("x-restaurant", opts.restaurant);
  }
  if (opts?.headers) {
    for (const [k, v] of Object.entries(opts.headers)) h.set(k, v);
  }
  return h;
}

async function parseProblem(res: Response): Promise<ProblemDetails> {
  const text = await res.text();
  if (!text) {
    return {
      type: "about:blank",
      title: res.statusText || "Error",
      status: res.status,
      detail: `HTTP ${res.status}`,
    };
  }
  try {
    const json = JSON.parse(text);
    if (json && typeof json.status === "number" && typeof json.title === "string") {
      return json as ProblemDetails;
    }
    return {
      type: "about:blank",
      title: res.statusText || "Error",
      status: res.status,
      detail: typeof json.detail === "string" ? json.detail : text,
      instance: json.instance,
      request_id: json.request_id,
    };
  } catch {
    return {
      type: "about:blank",
      title: res.statusText || "Error",
      status: res.status,
      detail: text.slice(0, 500),
    };
  }
}

async function apiFetch<T>(
  path: string,
  init: RequestInit & { opts?: RequestOptions } = {}
): Promise<T> {
  const url = path.startsWith("http") ? path : `${apiBase()}${path}`;
  const headers = buildHeaders(init.opts, !!init.body);
  // merge any headers from init
  if (init.headers) {
    const extra = new Headers(init.headers as HeadersInit);
    extra.forEach((v, k) => headers.set(k, v));
  }
  const res = await fetch(url, {
    ...init,
    headers,
    signal: init.opts?.signal ?? init.signal,
  });
  if (!res.ok) {
    const problem = await parseProblem(res);
    throw new ApiError(problem);
  }
  // 204
  if (res.status === 204) return undefined as unknown as T;
  const text = await res.text();
  if (!text) return undefined as unknown as T;
  return JSON.parse(text) as T;
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------
export interface HealthResponse { status: string; }
export interface ReadinessResponse {
  status: string;
  database: {
    connected: boolean;
    latency_ms?: number;
    pool_size?: number;
    idle_connections?: number;
    error?: string;
  };
}

export const healthApi = {
  /** GET /health — liveness, no deps */
  check(): Promise<HealthResponse> {
    return apiFetch<HealthResponse>("/health");
  },
  /** GET /health/ready — readiness, checks DB */
  ready(): Promise<ReadinessResponse> {
    return apiFetch<ReadinessResponse>("/health/ready");
  },
};

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------
export interface TableResponse {
  name: string;
  no_of_seats: number;
  minimum_seating: number;
  restaurant: string;
  restaurant_room: string;
  branch: string;
  is_take_away: boolean;
  occupied: boolean;
  table_shape: string | null;
  layout_x: number;
  layout_y: number;
  layout_width: number;
  layout_height: number;
  merged_with: string[];
}

export interface TableListResponse {
  count: number;
  tables: TableResponse[];
}

export interface MergeRequest { targets: string[]; }
export interface MergeResponse { cluster: string[]; count: number; }
export interface UnmergeResponse { removed: string; remaining: string[]; }
export interface TransferRequest { to_table: string; }
export interface TransferResponse { from_table: string; to_table: string; success: boolean; }

export interface TableListParams {
  room?: string;
  occupied?: boolean;
}

export const tablesApi = {
  list(params?: TableListParams, opts?: RequestOptions): Promise<TableListResponse> {
    const qs = new URLSearchParams();
    if (params?.room) qs.set("room", params.room);
    if (params?.occupied !== undefined) qs.set("occupied", String(params.occupied));
    const suffix = qs.toString() ? `?${qs}` : "";
    return apiFetch<TableListResponse>(`/api/tables${suffix}`, { opts });
  },
  get(id: string, opts?: RequestOptions): Promise<TableResponse> {
    return apiFetch<TableResponse>(`/api/tables/${encodeURIComponent(id)}`, { opts });
  },
  merge(id: string, req: MergeRequest, opts?: RequestOptions): Promise<MergeResponse> {
    return apiFetch<MergeResponse>(`/api/tables/${encodeURIComponent(id)}/merge`, {
      method: "POST",
      body: JSON.stringify(req),
      opts,
    });
  },
  unmerge(id: string, opts?: RequestOptions): Promise<UnmergeResponse> {
    return apiFetch<UnmergeResponse>(`/api/tables/${encodeURIComponent(id)}/unmerge`, {
      method: "POST",
      opts,
    });
  },
  transfer(id: string, req: TransferRequest, opts?: RequestOptions): Promise<TransferResponse> {
    return apiFetch<TransferResponse>(`/api/tables/${encodeURIComponent(id)}/transfer`, {
      method: "POST",
      body: JSON.stringify(req),
      opts,
    });
  },
};

// ---------------------------------------------------------------------------
// Menu + Items
// ---------------------------------------------------------------------------
export type MenuStrategy = "room" | "order_type" | "default";

export interface MenuItemResponse {
  item_code: string;
  item_name: string;
  /** Money as string (Decimal) */
  rate: MoneyString;
  special_dish: boolean;
  course: string | null;
  course_sequence: number | null;
}

export interface MenuResponse {
  restaurant: string;
  menu: string;
  strategy: MenuStrategy;
  fell_back: boolean;
  items: MenuItemResponse[];
}

export interface MenuItemsResponse {
  restaurant: string;
  menu: string;
  items: MenuItemResponse[];
}

export interface ItemDetailsResponse {
  item_code: string;
  item_name: string;
  item_group: string | null;
  stock_uom: string;
  is_bom: boolean;
  disabled: boolean;
}

export interface ItemPriceResponse {
  item_code: string;
  pricelist: string;
  /** Money as string */
  price: MoneyString;
}

export interface MenuQuery {
  room?: string;
  order_type?: string;
}

export const menuApi = {
  /** GET /api/menu?room=&order_type= — requires X-Restaurant */
  resolve(query: MenuQuery = {}, opts?: RequestOptions): Promise<MenuResponse> {
    const qs = new URLSearchParams();
    if (query.room) qs.set("room", query.room);
    if (query.order_type) qs.set("order_type", query.order_type);
    const suffix = qs.toString() ? `?${qs}` : "";
    return apiFetch<MenuResponse>(`/api/menu${suffix}`, { opts });
  },
  /** GET /api/menu/:menu_id/items — requires X-Restaurant */
  getItems(menuId: string, opts?: RequestOptions): Promise<MenuItemsResponse> {
    return apiFetch<MenuItemsResponse>(`/api/menu/${encodeURIComponent(menuId)}/items`, { opts });
  },
  // Legacy alias for docs/API.md POST /api/menu/resolve (now GET /api/menu)
  resolveLegacy(body: { room: string; items: string[] }, opts?: RequestOptions): Promise<unknown> {
    return apiFetch(`/api/menu/resolve`, { method: "POST", body: JSON.stringify(body), opts });
  },
  validate(body: { room: string; order_type: string; items: string[] }, opts?: RequestOptions): Promise<unknown> {
    return apiFetch(`/api/menu/validate`, { method: "POST", body: JSON.stringify(body), opts });
  },
};

export const itemsApi = {
  /** GET /api/items/:id */
  get(itemCode: string, opts?: RequestOptions): Promise<ItemDetailsResponse> {
    return apiFetch<ItemDetailsResponse>(`/api/items/${encodeURIComponent(itemCode)}`, { opts });
  },
  /** GET /api/items/:code/price?pricelist= */
  getPrice(itemCode: string, pricelist?: string, opts?: RequestOptions): Promise<ItemPriceResponse> {
    const qs = pricelist ? `?pricelist=${encodeURIComponent(pricelist)}` : "";
    return apiFetch<ItemPriceResponse>(`/api/items/${encodeURIComponent(itemCode)}/price${qs}`, { opts });
  },
};

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------
export interface OrderItemDto {
  item: string;
  item_name: string;
  qty: number;
  /** Money as string (Decimal) — accepts number|string on input, string on output */
  rate: MoneyString;
  comments?: string | null;
  serve_priority?: number;
  indicate_course?: boolean;
}

export interface CreateOrderRequest {
  take_away?: boolean;
  restaurant_table?: string | null;
  customer_name: string;
  no_of_pax?: number;
  waiter?: string | null;
  pos_profile?: string | null;
  cashier?: string | null;
  comments?: string | null;
  items?: OrderItemDto[];
}

export type OrderStatus = "open" | "invoiced" | "cancelled";

export interface OrderResponse {
  id: string;
  status: OrderStatus;
  version: number;
  take_away: boolean;
  restaurant_table: string | null;
  customer_name: string;
  no_of_pax: number;
  /** Money as string */
  grand_total: MoneyString;
  last_invoice: string | null;
  items: OrderItemDto[];
  waiter: string | null;
  pos_profile: string | null;
  cashier: string | null;
  comments: string | null;
  created_at: string;
  modified_at: string;
}

export interface PatchOrderRequest {
  items?: OrderItemDto[] | null;
  append_items?: OrderItemDto[] | null;
  no_of_pax?: number | null;
  customer_name?: string | null;
  comments?: string | null;
  waiter?: string | null;
  version?: number | null;
}

export interface CreateInvoiceFromOrderRequest {
  series: string;
  date: string; // YYYY-MM-DD
  branch: string;
  kot_naming_series?: string;
  room?: string | null;
}

export interface KotSummaryDto {
  id: string;
  production: string | null;
  kot_type: string;
  item_count: number;
  date: string;
}

export interface InvoiceFromOrderResponse {
  invoice_name: string;
  order_id: string;
  grand_total: MoneyString;
  rounded_total: MoneyString;
  round_off: MoneyString;
  status: string;
  fiscal_year: string;
  kots: KotSummaryDto[];
  unrouted_items: string[];
}

export interface CancelOrderResponse {
  id: string;
  status: OrderStatus;
  version: number;
}

export const ordersApi = {
  /** POST /api/orders — honours Idempotency-Key */
  create(req: CreateOrderRequest, opts?: RequestOptions): Promise<OrderResponse> {
    return apiFetch<OrderResponse>(`/api/orders`, {
      method: "POST",
      body: JSON.stringify(req),
      opts,
    });
  },
  /** GET /api/orders/:id */
  get(id: string, opts?: RequestOptions): Promise<OrderResponse> {
    return apiFetch<OrderResponse>(`/api/orders/${encodeURIComponent(id)}`, { opts });
  },
  /** PATCH /api/orders/:id */
  patch(id: string, req: PatchOrderRequest, opts?: RequestOptions): Promise<OrderResponse> {
    return apiFetch<OrderResponse>(`/api/orders/${encodeURIComponent(id)}`, {
      method: "PATCH",
      body: JSON.stringify(req),
      opts,
    });
  },
  /** DELETE /api/orders/:id */
  cancel(id: string, opts?: RequestOptions): Promise<CancelOrderResponse> {
    return apiFetch<CancelOrderResponse>(`/api/orders/${encodeURIComponent(id)}`, {
      method: "DELETE",
      opts,
    });
  },
  /** POST /api/orders/:id/invoice — honours Idempotency-Key */
  createInvoice(id: string, req: CreateInvoiceFromOrderRequest, opts?: RequestOptions): Promise<InvoiceFromOrderResponse> {
    return apiFetch<InvoiceFromOrderResponse>(`/api/orders/${encodeURIComponent(id)}/invoice`, {
      method: "POST",
      body: JSON.stringify(req),
      opts,
    });
  },
};

// ---------------------------------------------------------------------------
// KOT
// ---------------------------------------------------------------------------
export interface OrderLineDto {
  item_code: string;
  item_name: string;
  /** Decimal as string */
  qty: MoneyString;
  comments?: string | null;
  serve_priority?: number;
  indicate_course?: boolean;
}

export interface GenerateKotRequest {
  invoice: string;
  branch: string;
  naming_series: string;
  date: string; // YYYY-MM-DD
  time?: string | null;
  restaurant_table?: string | null;
  room?: string | null;
  customer_name?: string | null;
  pos_profile?: string | null;
  comments?: string | null;
  order_no?: string | null;
  table_takeaway?: boolean;
  is_aggregator?: boolean;
  aggregator_id?: string | null;
  items: OrderLineDto[];
}

export interface KotItemDto {
  item: string;
  item_name: string;
  quantity: MoneyString;
  cancelled_qty: MoneyString;
  comments: string | null;
  course: string | null;
  serve_priority: number;
  indicate_course: boolean;
}

export interface KotDto {
  id: string;
  naming_series: string;
  invoice: string;
  restaurant_table: string | null;
  customer_name: string | null;
  original_kot: string | null;
  date: string;
  time: string | null;
  kot_type: string;
  order_status: string | null;
  production: string | null;
  start_time_prep: string | null;
  items: KotItemDto[];
  pos_profile: string | null;
  branch: string | null;
  verified: boolean;
  verified_by: string | null;
  table_takeaway: boolean;
  is_aggregator: boolean;
  aggregator_id: string | null;
  comments: string | null;
  order_no: string | null;
}

export interface GenerateKotResponse {
  kots: KotDto[];
  unrouted_items: string[];
}

export interface PendingKotsResponse {
  production_unit: string;
  kots: KotDto[];
}

export interface MarkPreparedRequest {
  prepared_at?: string | null;
}

export const kotApi = {
  /** POST /api/kot/generate */
  generate(req: GenerateKotRequest, opts?: RequestOptions): Promise<GenerateKotResponse> {
    return apiFetch<GenerateKotResponse>(`/api/kot/generate`, {
      method: "POST",
      body: JSON.stringify(req),
      opts,
    });
  },
  /** GET /api/kot/:id */
  get(id: string, opts?: RequestOptions): Promise<KotDto> {
    return apiFetch<KotDto>(`/api/kot/${encodeURIComponent(id)}`, { opts });
  },
  /** GET /api/production-units/:unit_id/pending-kots */
  pending(unitId: string, opts?: RequestOptions): Promise<PendingKotsResponse> {
    return apiFetch<PendingKotsResponse>(
      `/api/production-units/${encodeURIComponent(unitId)}/pending-kots`,
      { opts }
    );
  },
  /** POST /api/kot/:id/mark-prepared */
  markPrepared(id: string, req: MarkPreparedRequest = {}, opts?: RequestOptions): Promise<KotDto> {
    return apiFetch<KotDto>(`/api/kot/${encodeURIComponent(id)}/mark-prepared`, {
      method: "POST",
      body: JSON.stringify(req),
      opts,
    });
  },
};

// ---------------------------------------------------------------------------
// Invoices + Payments
// ---------------------------------------------------------------------------
export type InvoiceStatusDto = "Draft" | "Paid" | "Consolidated" | "Return";
export type PaymentMethodDto = "Cash" | "Card" | "Upi" | "Wallet" | "Credit";
export type SupplyTypeDto = "Intrastate" | "Interstate";
export type DiscountBasisDto = "NetTotal" | "GrandTotal";

export interface InvoiceLineRequest {
  item_code: string;
  item_name: string;
  quantity: MoneyString;
  rate: MoneyString;
  hsn_sac?: string | null;
}

export interface CreateInvoiceRequest {
  order_id: string;
  table?: string | null;
  customer_name: string;
  lines: InvoiceLineRequest[];
  discount?: MoneyString;
  tax_rate: MoneyString;
  supply_type?: SupplyTypeDto;
  discount_basis?: DiscountBasisDto;
  series: string;
  posted_at?: string | null;
}

export interface InvoiceLineResponse {
  item_code: string;
  item_name: string;
  quantity: MoneyString;
  rate: MoneyString;
  amount: MoneyString;
  hsn_sac?: string | null;
}

export interface TaxBreakdownResponse {
  cgst: MoneyString;
  sgst: MoneyString;
  igst: MoneyString;
  total_tax: MoneyString;
}

export interface InvoiceTotalsResponse {
  net_total: MoneyString;
  discount: MoneyString;
  taxable_value: MoneyString;
  tax: TaxBreakdownResponse;
  grand_total: MoneyString;
  rounded_total: MoneyString;
  round_off: MoneyString;
}

export interface PaymentResponse {
  method: PaymentMethodDto;
  amount: MoneyString;
  reference?: string | null;
  paid_at: string;
}

export interface InvoiceResponse {
  invoice_id: string;
  order_id: string;
  table?: string | null;
  customer_name: string;
  status: InvoiceStatusDto;
  posted_at: string;
  business_day: string;
  fiscal_year: string;
  lines: InvoiceLineResponse[];
  net_total: MoneyString;
  discount: MoneyString;
  taxable_value: MoneyString;
  tax: TaxBreakdownResponse;
  grand_total: MoneyString;
  rounded_total: MoneyString;
  round_off: MoneyString;
  payments: PaymentResponse[];
  paid_amount: MoneyString;
  outstanding_amount: MoneyString;
  idempotency_key?: string | null;
}

export interface InvoiceSummaryResponse {
  invoice_id: string;
  order_id: string;
  table?: string | null;
  customer_name: string;
  status: InvoiceStatusDto;
  posted_at: string;
  business_day: string;
  grand_total: MoneyString;
  rounded_total: MoneyString;
  round_off: MoneyString;
  paid_amount: MoneyString;
  outstanding_amount: MoneyString;
}

export interface InvoiceListResponse {
  invoices: InvoiceSummaryResponse[];
  count: number;
  total_revenue: MoneyString;
}

export interface InvoiceListParams {
  from?: string; // YYYY-MM-DD business_day
  to?: string;
  status?: string;
  table?: string;
  business_day?: string; // legacy alias for from
}

export interface RecordPaymentRequest {
  method: PaymentMethodDto;
  amount: MoneyString;
  reference?: string | null;
  paid_at?: string | null;
}

export const invoicesApi = {
  /** POST /api/invoices — requires Idempotency-Key */
  create(req: CreateInvoiceRequest, opts?: RequestOptions): Promise<InvoiceResponse> {
    return apiFetch<InvoiceResponse>(`/api/invoices`, {
      method: "POST",
      body: JSON.stringify(req),
      opts,
    });
  },
  /** GET /api/invoices/:id */
  get(id: string, opts?: RequestOptions): Promise<InvoiceResponse> {
    return apiFetch<InvoiceResponse>(`/api/invoices/${encodeURIComponent(id)}`, { opts });
  },
  /** GET /api/invoices?from=&to=&status=&table= */
  list(params: InvoiceListParams = {}, opts?: RequestOptions): Promise<InvoiceListResponse> {
    const qs = new URLSearchParams();
    if (params.from) qs.set("from", params.from);
    if (params.to) qs.set("to", params.to);
    if (params.business_day) qs.set("business_day", params.business_day);
    if (params.status) qs.set("status", params.status);
    if (params.table) qs.set("table", params.table);
    const suffix = qs.toString() ? `?${qs}` : "";
    return apiFetch<InvoiceListResponse>(`/api/invoices${suffix}`, { opts });
  },
  /** POST /api/invoices/:id/pay */
  pay(id: string, req: RecordPaymentRequest, opts?: RequestOptions): Promise<InvoiceResponse> {
    return apiFetch<InvoiceResponse>(`/api/invoices/${encodeURIComponent(id)}/pay`, {
      method: "POST",
      body: JSON.stringify(req),
      opts,
    });
  },
  /** POST /api/invoices/:id/consolidate */
  consolidate(id: string, opts?: RequestOptions): Promise<InvoiceResponse> {
    return apiFetch<InvoiceResponse>(`/api/invoices/${encodeURIComponent(id)}/consolidate`, {
      method: "POST",
      opts,
    });
  },
};

// ---------------------------------------------------------------------------
// Shifts
// ---------------------------------------------------------------------------
export interface ShiftResponse {
  name: string;
  terminal: string;
  opened_at: string;
  closed_at: string | null;
  opened_by: string;
  business_day: string;
}

export interface ZReportResponse {
  shift_name: string;
  terminal: string;
  business_day: string;
  opened_at: string;
  closed_at: string;
  invoice_count: number;
  cash_total: MoneyString;
  card_total: MoneyString;
  total_revenue: MoneyString;
  cash_threshold_warning: boolean;
}

export interface ShiftListResponse { shifts: ShiftResponse[]; count: number; }

export interface OpenShiftRequest {
  terminal: string;
  opened_by: string;
  business_day?: string | null;
}

export interface CloseShiftRequest { cutoff_hour?: number; }
export interface ShiftListParams { terminal?: string; limit?: number; offset?: number; }

export const shiftsApi = {
  /** POST /api/shifts/open */
  open(req: OpenShiftRequest, opts?: RequestOptions): Promise<ShiftResponse> {
    return apiFetch<ShiftResponse>(`/api/shifts/open`, {
      method: "POST",
      body: JSON.stringify(req),
      opts,
    });
  },
  /** GET /api/shifts/current?terminal= */
  current(terminal: string, opts?: RequestOptions): Promise<ShiftResponse> {
    return apiFetch<ShiftResponse>(`/api/shifts/current?terminal=${encodeURIComponent(terminal)}`, { opts });
  },
  /** POST /api/shifts/:id/close */
  close(id: string, req: CloseShiftRequest = {}, opts?: RequestOptions): Promise<ZReportResponse> {
    return apiFetch<ZReportResponse>(`/api/shifts/${encodeURIComponent(id)}/close`, {
      method: "POST",
      body: JSON.stringify(req),
      opts,
    });
  },
  /** GET /api/shifts/:id/report */
  report(id: string, opts?: RequestOptions): Promise<ZReportResponse> {
    return apiFetch<ZReportResponse>(`/api/shifts/${encodeURIComponent(id)}/report`, { opts });
  },
  /** GET /api/shifts?terminal=&limit=&offset= */
  list(params: ShiftListParams = {}, opts?: RequestOptions): Promise<ShiftListResponse> {
    const qs = new URLSearchParams();
    if (params.terminal) qs.set("terminal", params.terminal);
    if (params.limit !== undefined) qs.set("limit", String(params.limit));
    if (params.offset !== undefined) qs.set("offset", String(params.offset));
    const suffix = qs.toString() ? `?${qs}` : "";
    return apiFetch<ShiftListResponse>(`/api/shifts${suffix}`, { opts });
  },
};

// ---------------------------------------------------------------------------
// COGS + Reports
// ---------------------------------------------------------------------------
export interface UnsetItems {
  item_prices: string[];
  bundle_items: string[];
  bom_items: string[];
}

export type CostBasis = "bundle" | "bom" | "plain";

export interface ItemCogsBreakdown {
  item_code: string;
  item_name: string;
  qty: MoneyString;
  cogs: MoneyString;
  cost_basis: CostBasis;
  unset: UnsetItems;
}

export interface CogsCalculateRequest {
  invoice?: string | null;
  from_date?: string | null;
  to_date?: string | null;
  cutoff_hour?: number;
}

export interface CogsCalculateResponse {
  scope: string;
  invoice?: string | null;
  from_date?: string | null;
  to_date?: string | null;
  invoice_count: number;
  cogs: MoneyString;
  items: ItemCogsBreakdown[];
  unset: UnsetItems;
  has_unset_items: boolean;
}

export interface DailyPlResponse {
  business_day: string;
  start: string;
  end: string;
  cutoff_hour: number;
  invoice_count: number;
  excluded_invoice_count: number;
  revenue: MoneyString;
  cogs: MoneyString;
  gross_profit: MoneyString;
  gross_margin_pct: string | null;
  round_off_total: MoneyString;
  unset: UnsetItems;
  has_unset_items: boolean;
}

export interface ItemCostingRow {
  item_code: string;
  item_name: string;
  qty_sold: MoneyString;
  revenue: MoneyString;
  cogs: MoneyString;
  gross_profit: MoneyString;
  cost_basis: CostBasis;
  unset: UnsetItems;
}

export interface ItemCostingResponse {
  business_day: string;
  start: string;
  end: string;
  cutoff_hour: number;
  invoice_count: number;
  items: ItemCostingRow[];
  total_cogs: MoneyString;
  total_line_revenue: MoneyString;
  unset: UnsetItems;
  has_unset_items: boolean;
}

export const cogsApi = {
  /** POST /api/cogs/calculate */
  calculate(req: CogsCalculateRequest, opts?: RequestOptions): Promise<CogsCalculateResponse> {
    return apiFetch<CogsCalculateResponse>(`/api/cogs/calculate`, {
      method: "POST",
      body: JSON.stringify(req),
      opts,
    });
  },
};

export const reportsApi = {
  /** GET /api/reports/daily-pl?date=&cutoff_hour= */
  dailyPl(params: { date?: string; cutoff_hour?: number }, opts?: RequestOptions): Promise<DailyPlResponse> {
    const qs = new URLSearchParams();
    if (params.date) qs.set("date", params.date);
    // also accept ?day= alias from API.md
    if ((params as unknown as { day?: string }).day) qs.set("date", (params as unknown as { day: string }).day);
    if (params.cutoff_hour !== undefined) qs.set("cutoff_hour", String(params.cutoff_hour));
    const suffix = qs.toString() ? `?${qs}` : "";
    return apiFetch<DailyPlResponse>(`/api/reports/daily-pl${suffix}`, { opts });
  },
  /** Alias for daily-pl with ?day= param (docs/API.md naming) */
  dailyPnl(params: { day: string }, opts?: RequestOptions): Promise<DailyPlResponse> {
    return apiFetch<DailyPlResponse>(`/api/reports/daily-pnl?day=${encodeURIComponent(params.day)}`, { opts })
      .catch(() => apiFetch<DailyPlResponse>(`/api/reports/daily-pl?date=${encodeURIComponent(params.day)}`, { opts }));
  },
  /** GET /api/reports/item-costing?date=&cutoff_hour= */
  itemCosting(params: { date?: string; cutoff_hour?: number }, opts?: RequestOptions): Promise<ItemCostingResponse> {
    const qs = new URLSearchParams();
    if (params.date) qs.set("date", params.date);
    if (params.cutoff_hour !== undefined) qs.set("cutoff_hour", String(params.cutoff_hour));
    const suffix = qs.toString() ? `?${qs}` : "";
    return apiFetch<ItemCostingResponse>(`/api/reports/item-costing${suffix}`, { opts });
  },
};

// ---------------------------------------------------------------------------
// Aggregator (Swiggy/Zomato webhooks)
// ---------------------------------------------------------------------------
export interface AggregatorItem {
  item_code: string;
  item_name: string;
  quantity: MoneyString;
  rate: MoneyString;
  special_instructions?: string | null;
}

export interface AggregatorWebhook {
  order_id: string;
  platform: string;
  customer_name: string;
  customer_phone?: string | null;
  items: AggregatorItem[];
  total: MoneyString;
  ordered_at: string;
  instructions?: string | null;
}

export interface WebhookResponse {
  status: string;
  order_id: string;
  internal_order_id?: string | null;
}

export type AggregatorOrderStatus = "pending" | "accepted" | "rejected" | "completed";

export interface AggregatorOrder {
  id: string;
  aggregator_order_id: string;
  platform: string;
  customer_name: string;
  customer_phone: string | null;
  items: AggregatorItem[];
  total: MoneyString;
  ordered_at: string;
  status: AggregatorOrderStatus;
  internal_order_id: string | null;
  internal_invoice_id: string | null;
  instructions: string | null;
  created_at: string;
  updated_at: string;
}

export interface AcceptOrderRequest { prep_time_minutes?: number | null; }
export interface AcceptOrderResponse { status: string; internal_order_id: string; message: string; }
export interface RejectOrderRequest { reason: string; }
export interface RejectOrderResponse { status: string; message: string; }

export interface Settlement {
  id: string;
  platform: string;
  settlement_date: string;
  total_orders: number;
  gross_amount: MoneyString;
  commission: MoneyString;
  net_amount: MoneyString;
  order_ids: string[];
}

export interface SettlementParams { date_from?: string; date_to?: string; platform?: string; }

export const aggregatorApi = {
  /** POST /api/aggregators/orders — webhook receiver (requires X-Webhook-Signature sha256= header) */
  webhook(req: AggregatorWebhook, signature: string, opts?: RequestOptions): Promise<WebhookResponse> {
    return apiFetch<WebhookResponse>(`/api/aggregators/orders`, {
      method: "POST",
      body: JSON.stringify(req),
      headers: { "X-Webhook-Signature": signature },
      opts,
    });
  },
  /** GET /api/aggregators/orders/:id */
  getOrder(id: string, opts?: RequestOptions): Promise<AggregatorOrder> {
    return apiFetch<AggregatorOrder>(`/api/aggregators/orders/${encodeURIComponent(id)}`, { opts });
  },
  /** POST /api/aggregators/orders/:id/accept */
  accept(id: string, req: AcceptOrderRequest = {}, opts?: RequestOptions): Promise<AcceptOrderResponse> {
    return apiFetch<AcceptOrderResponse>(`/api/aggregators/orders/${encodeURIComponent(id)}/accept`, {
      method: "POST",
      body: JSON.stringify(req),
      opts,
    });
  },
  /** POST /api/aggregators/orders/:id/reject */
  reject(id: string, req: RejectOrderRequest, opts?: RequestOptions): Promise<RejectOrderResponse> {
    return apiFetch<RejectOrderResponse>(`/api/aggregators/orders/${encodeURIComponent(id)}/reject`, {
      method: "POST",
      body: JSON.stringify(req),
      opts,
    });
  },
  /** GET /api/aggregators/settlements?date_from=&date_to=&platform= */
  settlements(params: SettlementParams = {}, opts?: RequestOptions): Promise<Settlement[]> {
    const qs = new URLSearchParams();
    if (params.date_from) qs.set("date_from", params.date_from);
    if (params.date_to) qs.set("date_to", params.date_to);
    if (params.platform) qs.set("platform", params.platform);
    const suffix = qs.toString() ? `?${qs}` : "";
    return apiFetch<Settlement[]>(`/api/aggregators/settlements${suffix}`, { opts });
  },
};

// ---------------------------------------------------------------------------
// SSE — prefer the hook for React; this is the raw URL helper
// ---------------------------------------------------------------------------
export function sseUrl(baseUrl?: string, params?: { events?: string[]; last_event_id?: string }): string {
  const base = (baseUrl ?? apiBase()).replace(/\/$/, "");
  const url = new URL(`${base}/api/events/stream`);
  if (params?.events?.length) url.searchParams.set("events", params.events.join(","));
  if (params?.last_event_id) url.searchParams.set("last_event_id", params.last_event_id);
  return url.toString();
}

// ---------------------------------------------------------------------------
// Convenience: generate an idempotency key (UUID v4)
// ---------------------------------------------------------------------------
export function newIdempotencyKey(): string {
  const gCrypto: Crypto | undefined =
    typeof globalThis !== "undefined" ? (globalThis as unknown as { crypto?: Crypto }).crypto : undefined;
  // Use crypto.randomUUID when available (browser + Node 19+)
  if (gCrypto && "randomUUID" in gCrypto) {
    return (gCrypto as unknown as { randomUUID: () => string }).randomUUID();
  }
  // fallback: RFC4122 v4 from getRandomValues
  const bytes = new Uint8Array(16);
  if (gCrypto) (gCrypto as unknown as { getRandomValues: (a: Uint8Array) => void }).getRandomValues(bytes); else {
    // Node fallback without Web Crypto (should not happen in Next runtime, but keep deterministic fallback)
    for (let i = 0; i < 16; i++) bytes[i] = Math.floor(Math.random() * 256);
  }
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

// Re-export money helpers for convenience
export * from "./money";
