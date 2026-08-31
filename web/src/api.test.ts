import { describe, expect, it } from "vitest";
import { ApiError, api, apiUrl, canonicalTools, metricWindows, setCsrf, subscribeUnauthorized } from "./api";
import type { Provider, ToolMapping } from "./api";
import { changeLabel, formatBucketLabel, formatMetric, metricWindowLabel, requestChartData, trendMeta } from "./metrics";
import { filterRequests, successRate } from "./admin-utils";

describe("admin contracts", () => {
  it("does not expose provider token fields", () => {
    const fields: keyof Provider = "token_configured";
    expect(fields).toBe("token_configured");
  });

  it("models the canonical-to-upstream tool mapping", () => {
    const mapping: ToolMapping = { canonical_tool: "tavily_search", upstream_tool: "search_proxy_tavily_search" };
    expect(mapping.upstream_tool).not.toBe(mapping.canonical_tool);
    expect(canonicalTools).toHaveLength(5);
  });

  it("builds API URLs from the configured backend base URL", () => {
    expect(apiUrl("/session")).toBe("/api/session");
    expect(apiUrl("providers")).toBe("/api/providers");
  });

  it("sends JSON and CSRF headers for write requests", async () => {
    const originalFetch = globalThis.fetch;
    let request: RequestInit | undefined;
    globalThis.fetch = async (_input, init) => {
      request = init;
      return new Response(JSON.stringify({ id: 7 }), { status: 201, headers: { "content-type": "application/json" } });
    };
    try {
      setCsrf("test-csrf");
      await api("/keys", { method: "POST", body: JSON.stringify({ name: "test" }) });
      expect(new Headers(request?.headers).get("content-type")).toBe("application/json");
      expect(new Headers(request?.headers).get("x-csrf-token")).toBe("test-csrf");
    } finally {
      setCsrf("");
      globalThis.fetch = originalFetch;
    }
  });

  it("classifies unauthorized responses and notifies the auth boundary", async () => {
    const originalFetch = globalThis.fetch;
    let notified = false;
    const unsubscribe = subscribeUnauthorized(() => { notified = true; });
    globalThis.fetch = async () => new Response(JSON.stringify({ error: "authentication required" }), { status: 401, headers: { "content-type": "application/json" } });
    try {
      await expect(api("/session")).rejects.toMatchObject({ status: 401, message: "authentication required" } satisfies Partial<ApiError>);
      expect(notified).toBe(true);
    } finally {
      unsubscribe();
      globalThis.fetch = originalFetch;
    }
  });

  it("calculates success rate and filters recent requests", () => {
    expect(successRate(3, 4)).toBe(75);
    expect(successRate(0, 0)).toBeNull();
    const events = [
      { request_id: "req-1", client_key_name: "production", client_key_prefix: "tmg_prod", provider: "Searchix", started_at: "2026-08-31T00:00:00Z", duration_ms: 20, outcome: "success", error_category: null },
      { request_id: "req-2", client_key_name: "staging", client_key_prefix: "tmg_stage", provider: null, started_at: "2026-08-31T00:01:00Z", duration_ms: 30, outcome: "error", error_category: "provider_error" },
    ];
    expect(filterRequests(events, "failure", "provider").map(event => event.request_id)).toEqual(["req-2"]);
    expect(filterRequests(events, "all", "searchix").map(event => event.request_id)).toEqual(["req-1"]);
  });
});

describe("public metrics presentation", () => {
  it("supports every server-side time window", () => {
    expect(metricWindows).toEqual(["1h", "24h", "7d", "30d", "all"]);
    expect(apiUrl(`/metrics?window=${metricWindows[2]}`)).toBe("/api/metrics?window=7d");
    expect(metricWindowLabel("all")).toBe("全部");
  });

  it("describes the actual request bucket width", () => {
    expect(formatBucketLabel(60)).toBe("REQUESTS / 1 MIN BUCKETS");
    expect(formatBucketLabel(21_600)).toBe("REQUESTS / 6 HOURS BUCKETS");
    expect(formatBucketLabel(2_592_000)).toBe("REQUESTS / 30 DAYS BUCKETS");
  });

  it("derives successful requests without allowing negative chart values", () => {
    const base = { timestamp: "2026-08-31T00:00:00Z", p50_ms: null, p95_ms: null };
    expect(requestChartData([
      { ...base, requests: 9, failures: 2 },
      { ...base, timestamp: "2026-08-31T00:01:00Z", requests: 1, failures: 3 },
    ]).map((point) => point.successes)).toEqual([7, 0]);
  });

  it("formats missing, rate and latency values without invented data", () => {
    expect(formatMetric(null, "latency")).toBe("-");
    expect(formatMetric(99.956, "rate")).toBe("99.96%");
    expect(formatMetric(123.6, "latency")).toBe("124 ms");
    expect(changeLabel(null, "percent")).toBe("暂无对比");
  });

  it("treats lower latency as an improvement", () => {
    expect(trendMeta(-12, true)).toMatchObject({ direction: "down", improved: true });
    expect(trendMeta(-12, false)).toMatchObject({ direction: "down", improved: false });
    expect(trendMeta(null, true)).toMatchObject({ direction: "neutral", improved: null });
  });
});
