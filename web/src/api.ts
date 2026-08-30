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

let csrf = "";
const csrfStorageKey = "tavily-gateway-csrf";
const apiBaseUrl = (import.meta.env.VITE_API_BASE_URL ?? "").trim().replace(/\/+$/, "");

function storedCsrfToken() {
  try {
    return window.localStorage.getItem(csrfStorageKey) ?? "";
  } catch {
    return "";
  }
}

export function setCsrf(value: string) {
  csrf = value;
  try {
    if (value) window.localStorage.setItem(csrfStorageKey, value);
    else window.localStorage.removeItem(csrfStorageKey);
  } catch {
    // The in-memory token still works when browser storage is unavailable.
  }
}

export function apiUrl(path: string) {
  const normalizedPath = path.startsWith("/") ? path : `/${path}`;
  return `${apiBaseUrl}/api${normalizedPath}`;
}

function csrfToken() {
  return csrf || storedCsrfToken() || document.cookie.split(";").map(value => value.trim()).find(value => value.startsWith("gateway_csrf="))?.slice("gateway_csrf=".length) || "";
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
  if (!response.ok) throw new Error(`Request failed (${response.status})`);
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
}
