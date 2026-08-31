export enum ProviderKind {
  Searchix = "searchix",
  TavilyHikari = "tavily_hikari",
}
export const canonicalTools = ["tavily_search", "tavily_extract", "tavily_crawl", "tavily_map", "tavily_research"] as const;
export type CanonicalTool = typeof canonicalTools[number];
export type ToolMapping = { canonical_tool: CanonicalTool; upstream_tool: string };
export type Provider = { id: number; kind: ProviderKind; name: string; endpoint: string; weight: number; enabled: boolean; connected: boolean; token_configured: boolean; tool_mappings: ToolMapping[] };
export type ClientKey = { id: number; name: string; prefix: string; status: string; created_at: string; last_used_at: string | null; request_count: number; key: string | null };
export type RequestEvent = { request_id: string; client_key_prefix: string | null; client_key_name: string | null; provider: string | null; started_at: string; duration_ms: number; outcome: string; error_category: string | null };
export type Metric = { requests: number; successes: number; failures: number; average_latency_ms: number };
export type OverviewData = Metric & { providers: Array<Metric & { provider: string }>; daily: Array<Metric & { day: string }> };
export type SessionData = { authenticated: true };
export type LoginResponse = { csrf_token: string; expires_at: string };
export type CreateKeyResponse = { id: number; prefix: string; key: string };
export type ProviderInput = { kind: ProviderKind; name: string; endpoint: string; token?: string; weight: number; enabled: boolean };

export const metricWindows = ["1h", "24h", "7d", "30d", "all"] as const;
export type MetricWindow = typeof metricWindows[number];
export type ComparedMetric<T> = { value: T; change: number | null };
export type MetricBucket = { timestamp: string; requests: number; failures: number; p50_ms: number | null; p95_ms: number | null };
export type ActivityBucket = { timestamp: string; requests: number };
export type LatencyBucket = { timestamp: string; p5_ms: number | null; p50_ms: number | null; p95_ms: number | null };
export type MetricsData = {
  window: MetricWindow;
  generated_at: string;
  timezone: "UTC";
  bucket_seconds: number;
  summary: {
    requests: ComparedMetric<number>;
    success_rate: ComparedMetric<number | null>;
    p50_ms: ComparedMetric<number | null>;
    p95_ms: ComparedMetric<number | null>;
  };
  series: MetricBucket[];
  activity: ActivityBucket[];
  latency_distribution: LatencyBucket[];
};

let csrf = "";
const unauthorizedListeners = new Set<() => void>();
const csrfStorageKey = "tavily-gateway-csrf";
const apiBaseUrl = (import.meta.env.VITE_API_BASE_URL ?? "").trim().replace(/\/+$/, "");

function storedCsrfToken() {
  try {
    if (typeof window === "undefined") return "";
    return window.localStorage.getItem(csrfStorageKey) ?? "";
  } catch {
    return "";
  }
}

export function setCsrf(value: string) {
  csrf = value;
  try {
    if (typeof window === "undefined") return;
    if (value) window.localStorage.setItem(csrfStorageKey, value);
    else window.localStorage.removeItem(csrfStorageKey);
  } catch {
    // The in-memory token still works when browser storage is unavailable.
  }
}

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
    this.name = "ApiError";
  }
}

export function subscribeUnauthorized(listener: () => void) {
  unauthorizedListeners.add(listener);
  return () => { unauthorizedListeners.delete(listener); };
}

export function isUnauthorized(error: unknown): error is ApiError {
  return error instanceof ApiError && error.status === 401;
}

export function apiUrl(path: string) {
  const normalizedPath = path.startsWith("/") ? path : `/${path}`;
  return `${apiBaseUrl}/api${normalizedPath}`;
}

function csrfToken() {
  const cookieToken = typeof document === "undefined" ? "" : document.cookie.split(";").map(value => value.trim()).find(value => value.startsWith("gateway_csrf="))?.slice("gateway_csrf=".length);
  return csrf || storedCsrfToken() || cookieToken || "";
}
export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const method = (init.method ?? "GET").toUpperCase();
  const headers = new Headers(init.headers);
  if (init.body !== undefined && init.body !== null && !headers.has("content-type")) {
    headers.set("content-type", "application/json");
  }
  if (!["GET", "HEAD", "OPTIONS"].includes(method) && !headers.has("x-csrf-token")) {
    headers.set("x-csrf-token", csrfToken());
  }
  const response = await fetch(apiUrl(path), { ...init, credentials: "include", headers });
  if (!response.ok) {
    const payload = await response.json().catch(() => null) as { error?: string } | null;
    if (response.status === 401) unauthorizedListeners.forEach(listener => listener());
    throw new ApiError(response.status, payload?.error ?? `Request failed (${response.status})`);
  }
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
}
