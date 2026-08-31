import type { MetricBucket, MetricWindow } from "./api";

const time = new Intl.DateTimeFormat("zh-CN", { timeZone: "Asia/Shanghai", hour: "2-digit", minute: "2-digit", hour12: false });
const dateTime = new Intl.DateTimeFormat("zh-CN", { timeZone: "Asia/Shanghai", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false });

export function formatCompact(value: number) {
  return new Intl.NumberFormat("zh-CN", { notation: "compact", maximumFractionDigits: 1 }).format(value);
}
export function formatMetric(value: number | null, kind: "requests" | "rate" | "latency") {
  if (value === null || !Number.isFinite(value)) return "-";
  if (kind === "requests") return formatCompact(value);
  if (kind === "rate") return `${value.toFixed(2)}%`;
  return `${Math.round(value)} ms`;
}
export function formatAxisTime(timestamp: string, window: MetricWindow) {
  return (window === "7d" || window === "30d" || window === "all" ? dateTime : time).format(new Date(timestamp));
}
export function formatFullTime(timestamp: string) {
  return `${dateTime.format(new Date(timestamp))} UTC+08:00`;
}
export function trendMeta(change: number | null, latency = false) {
  if (change === null || !Number.isFinite(change) || change === 0) return { direction: "neutral" as const, improved: null };
  return { direction: change > 0 ? "up" as const : "down" as const, improved: latency ? change < 0 : change > 0 };
}
export function changeLabel(change: number | null, kind: "percent" | "points" | "latency") {
  if (change === null) return "暂无对比";
  const sign = change > 0 ? "+" : "";
  if (kind === "percent") return `${sign}${change.toFixed(1)}%`;
  if (kind === "points") return `${sign}${change.toFixed(2)} pp`;
  return `${sign}${Math.round(change)} ms`;
}

export function metricWindowLabel(window: MetricWindow) {
  return window === "all" ? "全部" : window;
}

export function formatBucketLabel(bucketSeconds: number) {
  const units = [
    { seconds: 365 * 24 * 60 * 60, singular: "YEAR", plural: "YEARS" },
    { seconds: 24 * 60 * 60, singular: "DAY", plural: "DAYS" },
    { seconds: 60 * 60, singular: "HOUR", plural: "HOURS" },
    { seconds: 60, singular: "MIN", plural: "MINS" },
  ];
  const unit = units.find((candidate) => bucketSeconds % candidate.seconds === 0) ?? units[units.length - 1];
  const amount = Math.max(1, Math.round(bucketSeconds / unit.seconds));
  return `REQUESTS / ${amount} ${amount === 1 ? unit.singular : unit.plural} BUCKETS`;
}

export function requestChartData(series: MetricBucket[]) {
  return series.map((point) => ({ ...point, successes: Math.max(0, point.requests - point.failures) }));
}
