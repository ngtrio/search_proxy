import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { ArrowDown, ArrowUp, GearSix, Moon, Sun, WarningCircle } from "@phosphor-icons/react";
import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { Bar, BarChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";
import { api, metricWindows } from "./api";
import type { MetricWindow, MetricsData } from "./api";
import { changeLabel, formatAxisTime, formatBucketLabel, formatCompact, formatFullTime, formatMetric, metricWindowLabel, requestChartData, trendMeta } from "./metrics";
import { useTheme } from "./theme";
import type { Theme } from "./theme";

export function Dashboard() {
  const [window, setWindow] = useState<MetricWindow>("24h");
  const [theme, setTheme] = useTheme();
  const query = useQuery({
    queryKey: ["metrics", window],
    queryFn: () => api<MetricsData>(`/metrics?window=${window}`),
    placeholderData: keepPreviousData,
    refetchInterval: 60_000,
  });

  return <main className="dashboard-shell">
    <Header window={window} onWindow={setWindow} theme={theme} onTheme={() => setTheme(theme === "dark" ? "light" : "dark")} updatedAt={query.data?.generated_at} refreshing={query.isFetching && !query.isPending} />
    {query.isPending ? <DashboardSkeleton /> : query.isError ? <ErrorState retry={() => void query.refetch()} /> : query.data ? <DashboardBody data={query.data} window={window} /> : null}
  </main>;
}

function Header({ window, onWindow, theme, onTheme, updatedAt, refreshing }: { window: MetricWindow; onWindow: (value: MetricWindow) => void; theme: Theme; onTheme: () => void; updatedAt?: string; refreshing: boolean }) {
  return <header className="topbar">
    <div className="brand-block"><div className="brand-mark" aria-hidden="true"><span /><span /><span /></div><div><h1>Search Proxy</h1><p>请求流量监控 / Traffic observability</p></div></div>
    <div className="header-controls">
      <div className="window-tabs" aria-label="时间窗口">{metricWindows.map((item) => <button key={item} className={item === window ? "active" : ""} aria-pressed={item === window} onClick={() => onWindow(item)}>{metricWindowLabel(item)}</button>)}</div>
      <Link className="console-link" to="/admin" aria-label="进入控制台"><GearSix size={16} />控制台</Link>
      <button className="icon-button" onClick={onTheme} aria-label={theme === "dark" ? "切换至浅色主题" : "切换至深色主题"} title="切换主题">{theme === "dark" ? <Sun size={17} /> : <Moon size={17} />}</button>
      <div className="sync-state"><span className={refreshing ? "sync-pulse" : ""} />{updatedAt ? formatFullTime(updatedAt) : "同步中"}</div>
    </div>
  </header>;
}

function DashboardBody({ data, window }: { data: MetricsData; window: MetricWindow }) {
  const empty = data.series.every((point) => point.requests === 0);
  return <div className="dashboard-content"><MetricStrip data={data} window={window} />{empty ? <EmptyState /> : <TrafficChart data={data} window={window} />}<div className="lower-grid"><ActivityHeatmap data={data} /><LatencyDistribution data={data} /></div></div>;
}

function MetricStrip({ data, window }: { data: MetricsData; window: MetricWindow }) {
  const metrics = [
    { label: "请求量", en: "REQUESTS", value: formatMetric(data.summary.requests.value, "requests"), change: data.summary.requests.change, kind: "percent" as const, latency: false },
    { label: "成功率", en: "SUCCESS RATE", value: formatMetric(data.summary.success_rate.value, "rate"), change: data.summary.success_rate.change, kind: "points" as const, latency: false },
    { label: "P50 延迟", en: "MEDIAN LATENCY", value: formatMetric(data.summary.p50_ms.value, "latency"), change: data.summary.p50_ms.change, kind: "latency" as const, latency: true },
    { label: "P95 延迟", en: "TAIL LATENCY", value: formatMetric(data.summary.p95_ms.value, "latency"), change: data.summary.p95_ms.change, kind: "latency" as const, latency: true },
  ];
  return <section className="metric-strip" aria-label="关键指标">{metrics.map((metric) => {
    const trend = trendMeta(metric.change, metric.latency);
    const allTime = window === "all";
    return <article className="metric-cell" key={metric.en}><div className="metric-label"><span>{metric.label}</span><small>{metric.en}</small></div><strong>{metric.value}</strong><div className={`metric-change ${allTime ? "neutral" : trend.improved === true ? "good" : trend.improved === false ? "bad" : "neutral"}`}>{allTime ? "全部时间" : <>{trend.direction === "up" ? <ArrowUp size={12} /> : trend.direction === "down" ? <ArrowDown size={12} /> : null}{changeLabel(metric.change, metric.kind)} <span>对比上一周期</span></>}</div></article>;
  })}</section>;
}

function SectionHeading({ title, english, legend }: { title: string; english: string; legend?: React.ReactNode }) {
  return <div className="section-heading"><div><h2>{title}</h2><p>{english}</p></div>{legend}</div>;
}

function TrafficChart({ data, window }: { data: MetricsData; window: MetricWindow }) {
  const chartData = requestChartData(data.series).map((point) => ({ ...point, label: formatAxisTime(point.timestamp, window) }));
  return <section className="panel traffic-panel"><SectionHeading title="请求趋势" english={formatBucketLabel(data.bucket_seconds)} legend={<div className="legend"><span className="successes">成功</span><span className="failures">失败</span></div>} /><div className="chart-wrap" aria-label="按时间桶统计的成功与失败请求趋势图"><ResponsiveContainer width="100%" height="100%"><BarChart data={chartData} margin={{ top: 14, right: 0, bottom: 0, left: -12 }}>
    <CartesianGrid stroke="var(--grid)" vertical={false} /><XAxis dataKey="label" tick={{ fill: "var(--muted)", fontSize: 10 }} axisLine={false} tickLine={false} minTickGap={window === "1h" ? 48 : 72} /><YAxis tickFormatter={formatCompact} tick={{ fill: "var(--muted)", fontSize: 10 }} axisLine={false} tickLine={false} width={52} />
    <Tooltip content={<ChartTooltip />} cursor={{ fill: "var(--grid)", opacity: .45 }} /><Bar dataKey="successes" stackId="requests" fill="var(--gold)" opacity={.82} maxBarSize={10} isAnimationActive={false} /><Bar dataKey="failures" stackId="requests" fill="var(--red)" maxBarSize={10} isAnimationActive={false} />
  </BarChart></ResponsiveContainer></div></section>;
}

function ChartTooltip({ active, payload }: { active?: boolean; payload?: Array<{ payload: MetricsData["series"][number] & { successes: number } }> }) {
  const point = payload?.[0]?.payload; if (!active || !point) return null;
  return <div className="chart-tooltip"><time>{formatFullTime(point.timestamp)}</time><dl><div><dt>请求</dt><dd>{point.requests}</dd></div><div><dt>成功</dt><dd>{point.successes}</dd></div><div><dt>失败</dt><dd className="red">{point.failures}</dd></div></dl></div>;
}

function ActivityHeatmap({ data }: { data: MetricsData }) {
  const max = Math.max(...data.activity.map((point) => point.requests), 1);
  return <section className="panel heatmap-panel"><SectionHeading title="24 小时活跃度" english="ACTIVITY / 5 MIN BUCKETS" /><div className="heatmap-scroll"><div className="heatmap" role="img" aria-label="最近 24 小时请求活跃度热力图">{data.activity.map((point) => { const intensity = point.requests === 0 ? 0 : Math.max(.16, Math.log1p(point.requests) / Math.log1p(max)); return <span key={point.timestamp} style={{ "--intensity": intensity } as React.CSSProperties} title={`${formatFullTime(point.timestamp)}: ${point.requests} 请求`} />; })}</div></div><div className="heatmap-axis"><span>24h ago</span><span>18h</span><span>12h</span><span>6h</span><span>现在</span></div></section>;
}

function LatencyDistribution({ data }: { data: MetricsData }) {
  const max = Math.max(...data.latency_distribution.map((point) => point.p95_ms ?? 0), 1);
  return <section className="panel latency-panel"><SectionHeading title="近 24 小时延迟分布" english="HOURLY PERCENTILES / LAST 24H" legend={<div className="legend"><span className="p5">P5</span><span className="p50">P50</span><span className="p95">P95</span></div>} /><div className="latency-scroll"><div className="latency-columns">{data.latency_distribution.map((point, index) => {
    const p5 = ((point.p5_ms ?? 0) / max) * 100; const p50 = ((point.p50_ms ?? 0) / max) * 100; const p95 = ((point.p95_ms ?? 0) / max) * 100;
    return <div className="latency-column" key={point.timestamp} title={`${formatFullTime(point.timestamp)} P5 ${formatMetric(point.p5_ms, "latency")}, P50 ${formatMetric(point.p50_ms, "latency")}, P95 ${formatMetric(point.p95_ms, "latency")}`}>{point.p95_ms !== null && <><i style={{ bottom: `${p5}%`, height: `${Math.max(p95 - p5, 1)}%` }} /><b className="dot p5" style={{ bottom: `${p5}%` }} /><b className="dot p50" style={{ bottom: `${p50}%` }} /><b className="dot p95" style={{ bottom: `${p95}%` }} /></>}{index % 6 === 0 && <small>{formatAxisTime(point.timestamp, "24h")}</small>}</div>;
  })}</div></div></section>;
}

function DashboardSkeleton() { return <div className="dashboard-content skeleton" aria-label="正在加载指标"><div className="metric-strip">{[0,1,2,3].map((i) => <div className="metric-cell" key={i}><i /><b /><i /></div>)}</div><div className="panel skeleton-chart" /><div className="lower-grid"><div className="panel skeleton-small" /><div className="panel skeleton-small" /></div></div>; }
function ErrorState({ retry }: { retry: () => void }) { return <div className="state-panel"><WarningCircle size={28} /><h2>指标暂时不可用</h2><p>Metrics endpoint returned an error. 请检查服务状态后重试。</p><button onClick={retry}>重新加载</button></div>; }
function EmptyState() { return <section className="panel empty-state"><div className="empty-signal" aria-hidden="true"><span /><span /><span /><span /></div><h2>当前窗口暂无请求</h2><p>No traffic recorded in this window. 收到请求后，趋势图会自动出现。</p></section>; }
