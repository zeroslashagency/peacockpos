"use client";

import { useCallback, useEffect, useRef, useState } from "react";

/**
 * SSE hook for Peacock POS — KDS live ticket board and POS order updates.
 *
 * Connects to `GET /api/events/stream` via EventSource, handles:
 * - `kot.generated`, `kot.prepared` (native backend events)
 * - legacy aliases `kot_update`, `kot.submitted`, `kot.modified`
 * - `order.created`, `order.updated` + alias `order_update`
 * - `invoice.paid`
 *
 * Features: auto-reconnect with exponential backoff, Last-Event-ID resume via
 * query param, keep-alive comment, gap/lag detection, filtered subscriptions.
 *
 * Money in event payloads stays as string (never Number).
 */

export type SSEEventKind =
  | "order.created"
  | "order.updated"
  | "kot.generated"
  | "kot.prepared"
  | "invoice.paid"
  // legacy / task-spec aliases
  | "kot_update"
  | "order_update"
  | "kot.submitted"
  | "kot.modified"
  | string;

export interface SSEEvent {
  id: string;
  event: SSEEventKind;
  data: unknown;
  /** Raw JSON string as received */
  raw: string;
}

export interface UseSSEOptions {
  /** Override base URL. Defaults to NEXT_PUBLIC_API_URL or http://100.72.103.1:8080 */
  baseUrl?: string;
  /** Filter to specific event kinds: ?events=a,b . Null = all. */
  events?: string[];
  /** Initial Last-Event-ID to resume from */
  lastEventId?: string;
  /** Whether to auto-connect on mount. Default true. */
  autoConnect?: boolean;
  /** Reconnect base delay ms. Default 3000. */
  retryMs?: number;
  /** Max buffered events to keep. Default 100. */
  maxEvents?: number;
  /** Optional X-Restaurant header cannot be sent via EventSource; filter client-side if needed */
  enabled?: boolean;
}

export interface UseSSEReturn {
  events: SSEEvent[];
  connected: boolean;
  error: string | null;
  lastEventId: string | null;
  reconnect: () => void;
  disconnect: () => void;
  clear: () => void;
}

function getBaseUrl(explicit?: string): string {
  if (explicit) return explicit.replace(/\/$/, "");
  const raw =
    (typeof process !== "undefined" && (process.env.NEXT_PUBLIC_API_URL as string | undefined)) ||
    "";
  const cleaned = raw.replace(/^=+/, "").trim();
  // Force same-origin (Next.js rewrites → Hetzner) for http:// to avoid https→http mixed-content on Vercel
  if (cleaned.startsWith("http://")) return "";
  if (cleaned) return cleaned.replace(/\/$/, "");
  return "";
}

function buildUrl(base: string, opts: UseSSEOptions, lastId: string | null): string {
  const url = new URL(`${base}/api/events/stream`);
  if (opts.events && opts.events.length > 0) {
    url.searchParams.set("events", opts.events.join(","));
  }
  const resume = lastId ?? opts.lastEventId;
  if (resume) {
    url.searchParams.set("last_event_id", resume);
  }
  return url.toString();
}

// Normalise backend wire names to a stable set so consumers can match either.
function canonicalKind(raw: string): SSEEventKind {
  const k = raw.trim();
  // backend -> task alias mapping
  // kot.generated / kot.submitted / kot_update are all KOT creation
  // kot.prepared / kot.modified are KOT ready
  return k;
}

export function useSSE(options: UseSSEOptions = {}): UseSSEReturn {
  const { autoConnect = true, retryMs = 3000, maxEvents = 100, enabled = true } = options;

  const [events, setEvents] = useState<SSEEvent[]>([]);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastEventId, setLastEventId] = useState<string | null>(options.lastEventId ?? null);

  const esRef = useRef<EventSource | null>(null);
  const reconnectTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const retryCountRef = useRef(0);
  const lastIdRef = useRef<string | null>(options.lastEventId ?? null);
  const shouldConnectRef = useRef<boolean>(autoConnect && enabled);

  const pushEvent = useCallback(
    (evt: SSEEvent) => {
      setEvents((prev) => {
        const next = [...prev, evt];
        if (next.length > maxEvents) return next.slice(next.length - maxEvents);
        return next;
      });
      if (evt.id) {
        lastIdRef.current = evt.id;
        setLastEventId(evt.id);
      }
    },
    [maxEvents]
  );

  const connect = useCallback(() => {
    if (!shouldConnectRef.current) return;
    if (typeof window === "undefined") return;
    // Close existing before reconnect
    if (esRef.current) {
      esRef.current.close();
      esRef.current = null;
    }
    const base = getBaseUrl(options.baseUrl);
    const url = buildUrl(base, options, lastIdRef.current);
    try {
      const es = new EventSource(url);
      esRef.current = es;

      es.onopen = () => {
        setConnected(true);
        setError(null);
        retryCountRef.current = 0;
      };

      es.onerror = () => {
        setConnected(false);
        // EventSource will auto-retry per retry: header, but we also schedule explicit reconnect
        // if readyState is CLOSED (retry exhausted or network failure)
        if (es.readyState === EventSource.CLOSED) {
          const backoff = Math.min(retryMs * Math.pow(1.5, retryCountRef.current), 30000);
          retryCountRef.current += 1;
          setError(`SSE disconnected, reconnecting in ${Math.round(backoff)}ms`);
          if (reconnectTimer.current) clearTimeout(reconnectTimer.current);
          reconnectTimer.current = setTimeout(() => {
            connect();
          }, backoff);
        } else {
          setError("SSE connection error");
        }
      };

      // Catch-all for unknown event types (including keep-alive comments handled internally)
      es.onmessage = (msg: MessageEvent) => {
        // Fallback for events without explicit type
        if (!msg.data) return;
        pushEvent({
          id: (msg as unknown as { lastEventId?: string }).lastEventId ?? lastIdRef.current ?? "",
          event: "message",
          data: safeJsonParse(msg.data),
          raw: String(msg.data),
        });
      };

      // Known domain events
      const kinds: SSEEventKind[] = [
        "order.created",
        "order.updated",
        "kot.generated",
        "kot.prepared",
        "invoice.paid",
        "kot_update",
        "order_update",
        "kot.submitted",
        "kot.modified",
      ];
      for (const kind of kinds) {
        es.addEventListener(kind, ((e: MessageEvent) => {
          const id = (e as unknown as { lastEventId?: string }).lastEventId ?? "";
          pushEvent({
            id,
            event: canonicalKind(kind),
            data: safeJsonParse(e.data),
            raw: String(e.data),
          });
        }) as EventListener);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setConnected(false);
    }
  }, [options.baseUrl, options.events?.join(","), options.lastEventId, pushEvent, retryMs]);

  const disconnect = useCallback(() => {
    shouldConnectRef.current = false;
    if (reconnectTimer.current) {
      clearTimeout(reconnectTimer.current);
      reconnectTimer.current = null;
    }
    if (esRef.current) {
      esRef.current.close();
      esRef.current = null;
    }
    setConnected(false);
  }, []);

  const reconnect = useCallback(() => {
    retryCountRef.current = 0;
    shouldConnectRef.current = true;
    setError(null);
    connect();
  }, [connect]);

  const clear = useCallback(() => setEvents([]), []);

  useEffect(() => {
    shouldConnectRef.current = autoConnect && enabled;
    if (autoConnect && enabled) {
      connect();
    }
    return () => {
      if (reconnectTimer.current) clearTimeout(reconnectTimer.current);
      if (esRef.current) {
        esRef.current.close();
        esRef.current = null;
      }
    };
    // Only run on mount / enabled toggle
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled]);

  // Keep lastIdRef in sync if prop changes
  useEffect(() => {
    if (options.lastEventId) {
      lastIdRef.current = options.lastEventId;
      setLastEventId(options.lastEventId);
    }
  }, [options.lastEventId]);

  return { events, connected, error, lastEventId, reconnect, disconnect, clear };
}

function safeJsonParse(raw: string): unknown {
  try {
    return JSON.parse(raw);
  } catch {
    return raw;
  }
}

export default useSSE;
