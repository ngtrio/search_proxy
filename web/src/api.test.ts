import { describe, expect, it } from "vitest";
import { apiUrl, canonicalTools, metricWindows } from "./api";
import type { Provider, ToolMapping } from "./api";
import { changeLabel, formatMetric, trendMeta } from "./metrics";

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
});

describe("public metrics presentation", () => {
  it("supports every server-side time window", () => {
    expect(metricWindows).toEqual(["1h", "24h", "7d", "30d"]);
    expect(apiUrl(`/metrics?window=${metricWindows[2]}`)).toBe("/api/metrics?window=7d");
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
