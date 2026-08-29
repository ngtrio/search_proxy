export type Provider = { id: number; kind: "searchix" | "tavily_hikari"; name: string; endpoint: string; weight: number; enabled: boolean; timeout_seconds: number; token_configured: boolean };
export type ClientKey = { id: number; name: string; prefix: string; status: string; created_at: string; last_used_at: string | null; request_count: number };
export type RequestEvent = { request_id: string; client_key_prefix: string | null; client_key_name: string | null; provider: string | null; started_at: string; duration_ms: number; outcome: string; error_category: string | null };
export type Metric = { requests: number; successes: number; failures: number; average_latency_ms: number };
export type OverviewData = Metric & { providers: Array<Metric & { provider: string }>; daily: Array<Metric & { day: string }> };

let csrf = "";
export function setCsrf(value: string) { csrf = value; }
function csrfToken() {
  return csrf || document.cookie.split(";").map(value => value.trim()).find(value => value.startsWith("gateway_csrf="))?.slice("gateway_csrf=".length) || "";
}
export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const method = init.method ?? "GET";
  const response = await fetch(`/admin/api${path}`, { ...init, credentials: "same-origin", headers: { "content-type": "application/json", ...(method !== "GET" ? { "x-csrf-token": csrfToken() } : {}), ...init.headers } });
  if (!response.ok) throw new Error(`Request failed (${response.status})`);
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
}
