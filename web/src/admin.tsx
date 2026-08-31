import {
  ArrowSquareOut,
  ChartLineUp,
  Check,
  Copy,
  Eye,
  EyeSlash,
  Key,
  ListMagnifyingGlass,
  Moon,
  PencilSimple,
  PlugsConnected,
  Plus,
  SignOut,
  Sun,
  WarningCircle,
  X,
} from "@phosphor-icons/react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, Outlet, useLocation, useNavigate } from "@tanstack/react-router";
import { useEffect, useMemo, useRef, useState } from "react";
import { Bar, BarChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";
import {
  ApiError,
  ProviderKind,
  api,
  isUnauthorized,
  setCsrf,
  subscribeUnauthorized,
} from "./api";
import type {
  ClientKey,
  CreateKeyResponse,
  LoginResponse,
  OverviewData,
  Provider,
  ProviderInput,
  RequestEvent,
  SessionData,
} from "./api";
import { filterRequests, providerKindLabel, successRate } from "./admin-utils";
import type { RequestOutcomeFilter } from "./admin-utils";
import { formatFullTime } from "./metrics";
import { useTheme } from "./theme";

const adminQueryKeys = {
  session: ["admin", "session"] as const,
  overview: ["admin", "overview"] as const,
  providers: ["admin", "providers"] as const,
  keys: ["admin", "keys"] as const,
  requests: ["admin", "requests"] as const,
};

export function AdminRouteLayout() {
  const location = useLocation();
  if (location.pathname === "/admin/login") return <Outlet />;
  return <AdminGate />;
}

function AdminGate() {
  const navigate = useNavigate();
  const location = useLocation();
  const query = useQuery({
    queryKey: adminQueryKeys.session,
    queryFn: () => api<SessionData>("/session"),
    retry: false,
    staleTime: 5 * 60_000,
  });

  useEffect(() => {
    const login = () => {
      setCsrf("");
      void navigate({ to: "/admin/login", search: { redirect: location.pathname } });
    };
    const unsubscribe = subscribeUnauthorized(login);
    if (isUnauthorized(query.error)) login();
    return unsubscribe;
  }, [location.pathname, navigate, query.error]);

  if (query.isPending || isUnauthorized(query.error)) return <AdminLoading label="正在验证管理会话" />;
  if (query.isError) return <AdminStandaloneError title="无法验证管理会话" error={query.error} retry={() => void query.refetch()} />;
  return <AdminShell />;
}

function Brand() {
  return <div className="brand-block"><div className="brand-mark" aria-hidden="true"><span /><span /><span /></div><div><strong>Search Proxy</strong><p>CONTROL PLANE</p></div></div>;
}

function AdminShell() {
  const [theme, setTheme] = useTheme();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const logout = useMutation({
    mutationFn: () => api<void>("/logout", { method: "POST" }),
    onSuccess: () => {
      setCsrf("");
      queryClient.removeQueries({ queryKey: ["admin"] });
      void navigate({ to: "/admin/login", search: { redirect: "/admin" } });
    },
  });
  const nav = [
    { to: "/admin" as const, label: "概览", icon: ChartLineUp, exact: true },
    { to: "/admin/providers" as const, label: "供应商", icon: PlugsConnected },
    { to: "/admin/keys" as const, label: "客户端密钥", icon: Key },
    { to: "/admin/requests" as const, label: "请求记录", icon: ListMagnifyingGlass },
  ];

  return <main className="admin-shell">
    <aside className="admin-sidebar">
      <div className="admin-brand"><Brand /></div>
      <nav className="admin-nav" aria-label="控制台导航">{nav.map(item => {
        const Icon = item.icon;
        return <Link key={item.to} to={item.to} activeOptions={{ exact: item.exact }} activeProps={{ className: "active" }}><Icon size={17} />{item.label}</Link>;
      })}</nav>
      <div className="admin-sidebar-actions">
        <Link to="/" className="sidebar-action"><ArrowSquareOut size={16} />监控页</Link>
        <button className="sidebar-action" onClick={() => setTheme(theme === "dark" ? "light" : "dark")}><span>{theme === "dark" ? <Sun size={16} /> : <Moon size={16} />}</span>切换主题</button>
        <button className="sidebar-action" disabled={logout.isPending} onClick={() => logout.mutate()}><SignOut size={16} />{logout.isPending ? "正在退出" : "退出登录"}</button>
      </div>
    </aside>
    <section className="admin-main"><Outlet /></section>
  </main>;
}

export function AdminLogin({ redirect }: { redirect?: string }) {
  const [theme, setTheme] = useTheme();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const login = useMutation({
    mutationFn: () => api<LoginResponse>("/login", { method: "POST", body: JSON.stringify({ username, password }) }),
    onSuccess: async data => {
      setCsrf(data.csrf_token);
      await queryClient.invalidateQueries({ queryKey: adminQueryKeys.session });
      const target = redirect?.startsWith("/admin") && redirect !== "/admin/login" ? redirect : "/admin";
      void navigate({ to: target });
    },
  });

  return <main className="login-shell">
    <div className="login-top"><Brand /><button className="icon-button" onClick={() => setTheme(theme === "dark" ? "light" : "dark")} aria-label="切换主题">{theme === "dark" ? <Sun size={17} /> : <Moon size={17} />}</button></div>
    <section className="login-panel">
      <header><p>ADMIN ACCESS</p><h1>登录控制台</h1><span>使用部署时配置的管理员凭据。</span></header>
      <form onSubmit={event => { event.preventDefault(); login.mutate(); }}>
        <Field label="用户名" htmlFor="admin-username"><input id="admin-username" name="username" autoComplete="username" required maxLength={128} value={username} onChange={event => setUsername(event.target.value)} /></Field>
        <Field label="密码" htmlFor="admin-password"><input id="admin-password" name="password" type="password" autoComplete="current-password" required maxLength={1024} value={password} onChange={event => setPassword(event.target.value)} /></Field>
        {login.isError && <InlineError>{errorMessage(login.error, "登录失败，请检查用户名和密码。")}</InlineError>}
        <button className="primary-button login-submit" type="submit" disabled={login.isPending}>{login.isPending ? "正在验证" : "登录"}</button>
      </form>
      <Link to="/" className="login-back">返回公开监控页</Link>
    </section>
  </main>;
}

export function AdminOverview() {
  const query = useQuery({ queryKey: adminQueryKeys.overview, queryFn: () => api<OverviewData>("/overview"), refetchInterval: 60_000 });
  return <AdminPage title="运行概览" english="LAST 30 DAYS" description="管理流量、上游分布与服务表现。">
    {query.isPending ? <PageSkeleton rows={3} /> : query.isError ? <PageError error={query.error} retry={() => void query.refetch()} /> : <OverviewBody data={query.data} />}
  </AdminPage>;
}

function OverviewBody({ data }: { data: OverviewData }) {
  const rate = successRate(data.successes, data.requests);
  const metrics = [
    ["请求总量", data.requests.toLocaleString("zh-CN"), "REQUESTS"],
    ["成功率", rate === null ? "-" : `${rate.toFixed(2)}%`, "SUCCESS RATE"],
    ["失败请求", data.failures.toLocaleString("zh-CN"), "FAILURES"],
    ["平均延迟", `${data.average_latency_ms.toLocaleString("zh-CN")} ms`, "AVG LATENCY"],
  ];
  return <div className="admin-stack">
    <section className="admin-metric-strip" aria-label="30 天关键指标">{metrics.map(([label, value, english]) => <article key={english}><div><span>{label}</span><small>{english}</small></div><strong>{value}</strong></article>)}</section>
    <section className="admin-panel overview-chart-panel"><PanelHeading title="日请求趋势" english="DAILY REQUESTS" />
      {data.daily.length === 0 ? <CompactEmpty title="暂无流量数据" description="收到请求后，这里会显示最近 30 天趋势。" /> : <div className="overview-chart"><ResponsiveContainer width="100%" height="100%"><BarChart data={data.daily} margin={{ top: 16, right: 12, bottom: 0, left: -16 }}><CartesianGrid stroke="var(--grid)" vertical={false} /><XAxis dataKey="day" tick={{ fill: "var(--muted)", fontSize: 9 }} axisLine={false} tickLine={false} minTickGap={42} /><YAxis tick={{ fill: "var(--muted)", fontSize: 9 }} axisLine={false} tickLine={false} /><Tooltip content={<OverviewTooltip />} cursor={{ fill: "var(--grid)", opacity: .45 }} /><Bar dataKey="successes" stackId="daily" fill="var(--gold)" maxBarSize={14} isAnimationActive={false} /><Bar dataKey="failures" stackId="daily" fill="var(--red)" maxBarSize={14} isAnimationActive={false} /></BarChart></ResponsiveContainer></div>}
    </section>
    <section className="admin-panel"><PanelHeading title="供应商分布" english="PROVIDER BREAKDOWN" />
      {data.providers.length === 0 ? <CompactEmpty title="暂无供应商流量" description="成功路由请求后会显示上游使用情况。" /> : <div className="provider-breakdown">{data.providers.map(provider => { const rate = successRate(provider.successes, provider.requests); return <article key={provider.provider}><div><strong>{provider.provider}</strong><small>{provider.requests.toLocaleString("zh-CN")} 请求</small></div><dl><div><dt>成功率</dt><dd>{rate === null ? "-" : `${rate.toFixed(1)}%`}</dd></div><div><dt>失败</dt><dd className={provider.failures > 0 ? "bad-text" : ""}>{provider.failures}</dd></div><div><dt>平均延迟</dt><dd>{provider.average_latency_ms} ms</dd></div></dl></article>; })}</div>}
    </section>
  </div>;
}

function OverviewTooltip({ active, payload }: { active?: boolean; payload?: Array<{ payload: OverviewData["daily"][number] }> }) {
  const point = payload?.[0]?.payload;
  if (!active || !point) return null;
  return <div className="admin-tooltip"><time>{point.day}</time><span>成功 <b>{point.successes}</b></span><span>失败 <b className="bad-text">{point.failures}</b></span></div>;
}

export function AdminProviders() {
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: adminQueryKeys.providers, queryFn: () => api<Provider[]>("/providers") });
  const [editing, setEditing] = useState<Provider | "new" | null>(null);
  const save = useMutation({
    mutationFn: ({ provider, input }: { provider: Provider | "new"; input: ProviderInput }) => api<void | { id: number }>(provider === "new" ? "/providers" : `/providers/${provider.id}`, { method: provider === "new" ? "POST" : "PUT", body: JSON.stringify(input) }),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: adminQueryKeys.providers });
      setEditing(null);
    },
  });
  return <AdminPage title="供应商" english="UPSTREAM PROVIDERS" description="配置路由权重、连接状态与 Tavily 工具映射。" action={<button className="primary-button" onClick={() => setEditing("new")}><Plus size={15} />新增供应商</button>}>
    {query.isPending ? <PageSkeleton rows={4} /> : query.isError ? <PageError error={query.error} retry={() => void query.refetch()} /> : query.data.length === 0 ? <PageEmpty title="尚未配置供应商" description="新增 Searchix 或 Tavily Hikari 上游后即可开始路由请求。" action={<button className="primary-button" onClick={() => setEditing("new")}><Plus size={15} />新增供应商</button>} /> : <div className="provider-list">{query.data.map(provider => <ProviderRow key={provider.id} provider={provider} edit={() => setEditing(provider)} />)}</div>}
    {editing && <Modal title={editing === "new" ? "新增供应商" : "编辑供应商"} close={() => !save.isPending && setEditing(null)}><ProviderForm provider={editing} pending={save.isPending} error={save.error} submit={input => save.mutate({ provider: editing, input })} cancel={() => setEditing(null)} /></Modal>}
  </AdminPage>;
}

function ProviderRow({ provider, edit }: { provider: Provider; edit: () => void }) {
  return <article className="provider-row">
    <div className="provider-main"><div><strong>{provider.name}</strong><span>{providerKindLabel(provider.kind)}</span></div><code>{provider.endpoint}</code></div>
    <div className="provider-status"><StatusBadge tone={!provider.enabled ? "neutral" : provider.connected ? "good" : "bad"}>{!provider.enabled ? "已停用" : provider.connected ? "已连接" : "未连接"}</StatusBadge><span>权重 {provider.weight}</span></div>
    <details className="tool-mappings"><summary>{provider.tool_mappings.length} 个工具映射</summary>{provider.tool_mappings.length === 0 ? <p>当前没有已发现的工具。</p> : <dl>{provider.tool_mappings.map(mapping => <div key={mapping.canonical_tool}><dt>{mapping.canonical_tool}</dt><dd>{mapping.upstream_tool}</dd></div>)}</dl>}</details>
    <button className="row-action" onClick={edit}><PencilSimple size={15} />编辑</button>
  </article>;
}

function ProviderForm({ provider, pending, error, submit, cancel }: { provider: Provider | "new"; pending: boolean; error: unknown; submit: (input: ProviderInput) => void; cancel: () => void }) {
  const current = provider === "new" ? null : provider;
  const [kind, setKind] = useState<ProviderKind>(current?.kind ?? ProviderKind.Searchix);
  const [name, setName] = useState(current?.name ?? "");
  const [endpoint, setEndpoint] = useState(current?.endpoint ?? "");
  const [token, setToken] = useState("");
  const [weight, setWeight] = useState(current?.weight ?? 1);
  const [enabled, setEnabled] = useState(current?.enabled ?? true);
  return <form className="control-form" onSubmit={event => { event.preventDefault(); submit({ kind, name: name.trim(), endpoint: endpoint.trim(), token: token || undefined, weight, enabled }); }}>
    <div className="form-grid"><Field label="供应商类型" htmlFor="provider-kind"><select id="provider-kind" value={kind} onChange={event => setKind(event.target.value as ProviderKind)}><option value={ProviderKind.Searchix}>Searchix</option><option value={ProviderKind.TavilyHikari}>Tavily Hikari</option></select></Field><Field label="名称" htmlFor="provider-name"><input id="provider-name" required value={name} onChange={event => setName(event.target.value)} /></Field></div>
    <Field label="MCP 端点" htmlFor="provider-endpoint" helper="必须是可访问的 HTTP 或 HTTPS 地址。"><input id="provider-endpoint" type="url" required placeholder="https://example.com/mcp" value={endpoint} onChange={event => setEndpoint(event.target.value)} /></Field>
    <div className="form-grid"><Field label={current ? "访问令牌（可选）" : "访问令牌"} htmlFor="provider-token" helper={current ? "留空将保留当前令牌。" : undefined}><input id="provider-token" type="password" autoComplete="new-password" required={!current} value={token} onChange={event => setToken(event.target.value)} /></Field><Field label="路由权重" htmlFor="provider-weight" helper="正整数，数值越高获得的请求越多。"><input id="provider-weight" type="number" min={1} step={1} required value={weight} onChange={event => setWeight(Number(event.target.value))} /></Field></div>
    <label className="check-field"><input type="checkbox" checked={enabled} onChange={event => setEnabled(event.target.checked)} /><span><b>启用供应商</b><small>启用时保存会验证连接和工具目录，可能需要一些时间。</small></span></label>
    {Boolean(error) && <InlineError>{providerErrorMessage(error)}</InlineError>}
    <div className="form-actions"><button type="button" className="secondary-button" disabled={pending} onClick={cancel}>取消</button><button type="submit" className="primary-button" disabled={pending}>{pending ? "正在验证并保存" : "保存供应商"}</button></div>
  </form>;
}

export function AdminKeys() {
  const queryClient = useQueryClient();
  const query = useQuery({ queryKey: adminQueryKeys.keys, queryFn: () => api<ClientKey[]>("/keys") });
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [visible, setVisible] = useState<Set<number>>(new Set());
  const [copied, setCopied] = useState<number | null>(null);
  const create = useMutation({
    mutationFn: () => api<CreateKeyResponse>("/keys", { method: "POST", body: JSON.stringify({ name: name.trim() }) }),
    onSuccess: async data => {
      await queryClient.invalidateQueries({ queryKey: adminQueryKeys.keys });
      setVisible(current => new Set(current).add(data.id));
      setCreating(false);
      setName("");
    },
  });
  const copy = async (id: number, key: string) => {
    try {
      await navigator.clipboard.writeText(key);
      setCopied(id);
      window.setTimeout(() => setCopied(current => current === id ? null : current), 1600);
    } catch {
      setCopied(null);
    }
  };
  return <AdminPage title="客户端密钥" english="CLIENT API KEYS" description="查看调用凭据、使用情况并创建新的访问密钥。" action={<button className="primary-button" onClick={() => setCreating(true)}><Plus size={15} />创建密钥</button>}>
    {query.isPending ? <PageSkeleton rows={4} /> : query.isError ? <PageError error={query.error} retry={() => void query.refetch()} /> : query.data.length === 0 ? <PageEmpty title="尚未创建客户端密钥" description="创建密钥后，客户端即可通过 Bearer token 调用 MCP 端点。" action={<button className="primary-button" onClick={() => setCreating(true)}><Plus size={15} />创建密钥</button>} /> : <div className="key-list">{query.data.map(item => { const shown = visible.has(item.id); return <article className="key-row" key={item.id}><div className="key-identity"><strong>{item.name}</strong><code>{item.prefix}</code></div><div className="key-secret"><code>{item.key ? shown ? item.key : maskSecret(item.key) : "不可用"}</code>{item.key && <><button onClick={() => setVisible(current => { const next = new Set(current); shown ? next.delete(item.id) : next.add(item.id); return next; })} aria-label={shown ? "隐藏密钥" : "显示密钥"}>{shown ? <EyeSlash size={15} /> : <Eye size={15} />}</button><button onClick={() => void copy(item.id, item.key!)} aria-label="复制密钥">{copied === item.id ? <Check size={15} /> : <Copy size={15} />}</button></>}</div><dl className="key-meta"><div><dt>创建时间</dt><dd>{formatFullTime(item.created_at)}</dd></div><div><dt>最后使用</dt><dd>{item.last_used_at ? formatFullTime(item.last_used_at) : "从未使用"}</dd></div><div><dt>请求数</dt><dd>{item.request_count.toLocaleString("zh-CN")}</dd></div></dl><StatusBadge tone={item.status === "active" ? "good" : "neutral"}>{item.status === "active" ? "有效" : item.status}</StatusBadge></article>; })}</div>}
    {creating && <Modal title="创建客户端密钥" close={() => !create.isPending && setCreating(false)}><form className="control-form" onSubmit={event => { event.preventDefault(); create.mutate(); }}><Field label="密钥名称" htmlFor="key-name" helper="使用能识别调用方或环境的名称。"><input id="key-name" autoFocus required value={name} onChange={event => setName(event.target.value)} placeholder="例如：生产搜索服务" /></Field>{create.isError && <InlineError>{errorMessage(create.error, "无法创建客户端密钥。")}</InlineError>}<div className="form-actions"><button type="button" className="secondary-button" disabled={create.isPending} onClick={() => setCreating(false)}>取消</button><button className="primary-button" type="submit" disabled={create.isPending || !name.trim()}>{create.isPending ? "正在创建" : "创建密钥"}</button></div></form></Modal>}
  </AdminPage>;
}

export function AdminRequests() {
  const query = useQuery({ queryKey: adminQueryKeys.requests, queryFn: () => api<RequestEvent[]>("/requests"), refetchInterval: 60_000 });
  const [outcome, setOutcome] = useState<RequestOutcomeFilter>("all");
  const [search, setSearch] = useState("");
  const filtered = useMemo(() => filterRequests(query.data ?? [], outcome, search), [outcome, query.data, search]);
  return <AdminPage title="请求记录" english="RECENT 200 REQUESTS" description="检查最近调用的路由、耗时和结果分类。" action={query.data ? <span className="result-count">显示 {filtered.length} / {query.data.length}</span> : undefined}>
    <div className="request-filters"><label><span>搜索</span><input type="search" value={search} onChange={event => setSearch(event.target.value)} placeholder="请求 ID、密钥或供应商" /></label><label><span>结果</span><select value={outcome} onChange={event => setOutcome(event.target.value as RequestOutcomeFilter)}><option value="all">全部结果</option><option value="success">仅成功</option><option value="failure">仅失败</option></select></label></div>
    {query.isPending ? <PageSkeleton rows={7} /> : query.isError ? <PageError error={query.error} retry={() => void query.refetch()} /> : query.data.length === 0 ? <PageEmpty title="暂无请求记录" description="客户端发起请求后，最近 200 条记录会显示在这里。" /> : filtered.length === 0 ? <CompactEmpty title="没有匹配的请求" description="调整搜索词或结果筛选条件。" /> : <div className="request-table-wrap"><table className="request-table"><thead><tr><th>时间</th><th>请求 ID</th><th>客户端</th><th>供应商</th><th>耗时</th><th>结果</th></tr></thead><tbody>{filtered.map(event => <tr key={event.request_id}><td data-label="时间"><time>{formatFullTime(event.started_at)}</time></td><td data-label="请求 ID"><code title={event.request_id}>{event.request_id}</code></td><td data-label="客户端"><strong>{event.client_key_name ?? "未知客户端"}</strong><small>{event.client_key_prefix ?? "-"}</small></td><td data-label="供应商">{event.provider ?? "未路由"}</td><td data-label="耗时"><span className="mono-value">{event.duration_ms} ms</span></td><td data-label="结果"><StatusBadge tone={event.outcome === "success" ? "good" : "bad"}>{event.outcome === "success" ? "成功" : event.error_category ?? "失败"}</StatusBadge></td></tr>)}</tbody></table></div>}
  </AdminPage>;
}

function AdminPage({ title, english, description, action, children }: { title: string; english: string; description: string; action?: React.ReactNode; children: React.ReactNode }) {
  return <div className="admin-page"><header className="admin-page-header"><div><p>{english}</p><h1>{title}</h1><span>{description}</span></div>{action && <div className="admin-page-action">{action}</div>}</header>{children}</div>;
}

function PanelHeading({ title, english }: { title: string; english: string }) { return <header className="panel-heading"><div><h2>{title}</h2><p>{english}</p></div></header>; }
function Field({ label, htmlFor, helper, children }: { label: string; htmlFor: string; helper?: string; children: React.ReactNode }) { return <label className="field" htmlFor={htmlFor}><span>{label}</span>{children}{helper && <small>{helper}</small>}</label>; }
function StatusBadge({ tone, children }: { tone: "good" | "bad" | "neutral"; children: React.ReactNode }) { return <span className={`status-badge ${tone}`}>{children}</span>; }
function InlineError({ children }: { children: React.ReactNode }) { return <div className="inline-error" role="alert"><WarningCircle size={16} />{children}</div>; }

function Modal({ title, close, children }: { title: string; close: () => void; children: React.ReactNode }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => { ref.current?.showModal(); }, []);
  return <dialog className="admin-dialog" ref={ref} onCancel={event => { event.preventDefault(); close(); }} onClick={event => { if (event.target === event.currentTarget) close(); }}><div className="dialog-content"><header><h2>{title}</h2><button onClick={close} aria-label="关闭"><X size={17} /></button></header>{children}</div></dialog>;
}

function PageSkeleton({ rows }: { rows: number }) { return <div className="admin-page-skeleton" aria-label="正在加载">{Array.from({ length: rows }, (_, index) => <span key={index} />)}</div>; }
function PageError({ error, retry }: { error: unknown; retry: () => void }) { return <div className="page-state error"><WarningCircle size={24} /><h2>数据暂时不可用</h2><p>{errorMessage(error, "请检查服务状态后重试。")}</p><button className="secondary-button" onClick={retry}>重新加载</button></div>; }
function PageEmpty({ title, description, action }: { title: string; description: string; action?: React.ReactNode }) { return <div className="page-state empty"><PlugsConnected size={25} /><h2>{title}</h2><p>{description}</p>{action}</div>; }
function CompactEmpty({ title, description }: { title: string; description: string }) { return <div className="compact-empty"><strong>{title}</strong><p>{description}</p></div>; }
function AdminLoading({ label }: { label: string }) { return <main className="login-shell"><div className="admin-auth-state"><Brand /><div className="loading-bars" aria-label={label}><span /><span /><span /></div><p>{label}</p></div></main>; }
function AdminStandaloneError({ title, error, retry }: { title: string; error: unknown; retry: () => void }) { return <main className="login-shell"><div className="admin-auth-state"><WarningCircle size={27} /><h1>{title}</h1><p>{errorMessage(error, "请确认后端服务可用。")}</p><button className="secondary-button" onClick={retry}>重新尝试</button></div></main>; }

function maskSecret(secret: string) { return `${secret.slice(0, 12)}${"•".repeat(Math.min(24, Math.max(secret.length - 12, 8)))}`; }
function errorMessage(error: unknown, fallback: string) { return error instanceof ApiError ? error.message : fallback; }
function providerErrorMessage(error: unknown) {
  if (!(error instanceof ApiError)) return "无法保存供应商，请稍后重试。";
  if (error.status === 400) return "供应商配置无效，请检查名称、端点、令牌和权重。";
  if (error.status === 502) return "无法连接供应商，请确认端点、令牌和服务状态。";
  return error.message;
}
