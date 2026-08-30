import React, { FormEvent, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { api, canonicalTools, ClientKey, OverviewData, Provider, ProviderKind, RequestEvent, setCsrf } from "./api";
import "./styles.css";

type Page = "overview" | "providers" | "keys" | "requests";
type ProviderForm = Pick<Provider, "kind" | "name" | "endpoint" | "weight" | "enabled"> & { token: string };
const emptyProvider: ProviderForm = { kind: ProviderKind.Searchix, name: "", endpoint: "https://", token: "", weight: 1, enabled: true };

function Login({ done }: { done: () => void }) {
  const [error, setError] = useState("");
  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault(); const data = new FormData(event.currentTarget);
    try { const result = await api<{ csrf_token: string }>("/login", { method: "POST", body: JSON.stringify({ username: data.get("username"), password: data.get("password") }) }); setCsrf(result.csrf_token); done(); }
    catch { setError("Invalid credentials"); }
  }
  return <main className="login"><form onSubmit={submit}><p className="eyebrow">CONTROL PLANE</p><h1>Tavily Gateway</h1><label>Username<input name="username" autoComplete="username" required /></label><label>Password<input name="password" type="password" autoComplete="current-password" required /></label>{error && <p className="error">{error}</p>}<button>Sign in</button></form></main>;
}

function Panel({ signedOut }: { signedOut: () => void }) {
  const [page, setPage] = useState<Page>("overview");
  async function logout() { await api("/logout", { method: "POST", body: "{}" }); setCsrf(""); signedOut(); }
  return <div className="shell"><aside><div><p className="eyebrow">TAVILY</p><h2>Gateway</h2></div><nav>{(["overview", "providers", "keys", "requests"] as Page[]).map(value => <button className={value === page ? "active" : ""} onClick={() => setPage(value)} key={value}>{value}</button>)}</nav><button className="signout" onClick={logout}>Sign out</button><small>Metadata only · no query retention</small></aside><main><header><p className="eyebrow">OPERATIONS</p><h1>{page[0].toUpperCase() + page.slice(1)}</h1></header>{page === "overview" ? <Overview /> : page === "providers" ? <Providers /> : page === "keys" ? <Keys /> : <Requests />}</main></div>;
}

function Overview() {
  const [data, setData] = useState<OverviewData>({ requests: 0, successes: 0, failures: 0, average_latency_ms: 0, providers: [], daily: [] });
  useEffect(() => { void api<OverviewData>("/overview").then(setData); }, []);
  const totals = { requests: data.requests, successes: data.successes, failures: data.failures, average_latency_ms: data.average_latency_ms };
  return <section><div className="cards">{Object.entries(totals).map(([key, value]) => <article key={key}><span>{key.replaceAll("_", " ")}</span><strong>{value}</strong></article>)}</div><h3>Provider performance</h3><div className="table"><div className="tr metric head"><span>Provider</span><span>Requests</span><span>Success</span><span>Failure</span><span>Avg latency</span></div>{data.providers.map(row => <div className="tr metric" key={row.provider}><span>{row.provider}</span><span>{row.requests}</span><span className="ok">{row.successes}</span><span className="warn">{row.failures}</span><span>{row.average_latency_ms} ms</span></div>)}</div><h3>30-day latency trend</h3><div className="table"><div className="tr metric head"><span>Day</span><span>Requests</span><span>Success</span><span>Failure</span><span>Avg latency</span></div>{data.daily.map(row => <div className="tr metric" key={row.day}><span>{row.day}</span><span>{row.requests}</span><span className="ok">{row.successes}</span><span className="warn">{row.failures}</span><span>{row.average_latency_ms} ms</span></div>)}</div></section>;
}

function Providers() {
  const [rows, setRows] = useState<Provider[]>([]); const [editing, setEditing] = useState<number | null>(null); const [form, setForm] = useState<ProviderForm>(emptyProvider);
  const load = () => api<Provider[]>("/providers").then(setRows); useEffect(() => { void load(); }, []);
  function edit(provider?: Provider) { setEditing(provider?.id ?? 0); setForm(provider ? { kind: provider.kind, name: provider.name, endpoint: provider.endpoint, token: "", weight: provider.weight, enabled: provider.enabled } : emptyProvider); }
  async function save(event: FormEvent) { event.preventDefault(); const path = editing ? `/providers/${editing}` : "/providers"; await api(path, { method: editing ? "PUT" : "POST", body: JSON.stringify(form) }); setEditing(null); await load(); }
  return <section>
    <button onClick={() => edit()}>Add provider</button>
    {editing !== null && <form className="editor" onSubmit={save}><label>Name<input value={form.name} onChange={e => setForm({ ...form, name: e.target.value })} required /></label><label>Adapter<select value={form.kind} onChange={e => setForm({ ...form, kind: e.target.value as ProviderKind })}><option value={ProviderKind.Searchix}>Searchix</option><option value={ProviderKind.TavilyHikari}>Tavily Hikari</option></select></label><label>Endpoint<input type="url" value={form.endpoint} onChange={e => setForm({ ...form, endpoint: e.target.value })} required /></label><label>Token (write-only)<input type="password" value={form.token} onChange={e => setForm({ ...form, token: e.target.value })} required={!editing} placeholder={editing ? "Leave blank to retain" : "Required"} /></label><label>Weight<input type="number" min="1" value={form.weight} onChange={e => setForm({ ...form, weight: Number(e.target.value) })} /></label><label className="check"><input type="checkbox" checked={form.enabled} onChange={e => setForm({ ...form, enabled: e.target.checked })} /> Enabled</label><div><button>Save</button> <button type="button" className="secondary" onClick={() => setEditing(null)}>Cancel</button></div></form>}
    <div className="table"><div className="tr head"><span>Name</span><span>Adapter</span><span>Status</span><span>Weight</span><span /></div>{rows.map(provider => { const status = !provider.enabled ? "disabled" : provider.connected ? "connected" : "unavailable"; return <div className="tr" key={provider.id}><span>{provider.name}<small>{provider.endpoint}</small></span><span>{provider.kind}</span><span className={provider.connected ? "ok" : "warn"}>{status}</span><span>{provider.weight}</span><span><button className="secondary" onClick={() => edit(provider)}>Edit</button></span></div>; })}</div>
    <ToolMappingMatrix providers={rows} />
    <p className="note">Mappings come from each connected provider's live tools/list response. Provider tokens are write-only.</p>
  </section>;
}

function ToolMappingMatrix({ providers }: { providers: Provider[] }) {
  const routeCount = providers.reduce((total, provider) => total + provider.tool_mappings.length, 0);
  const connectedCount = providers.filter(provider => provider.connected).length;
  return <section className="tool-map" aria-labelledby="tool-map-title">
    <div className="tool-map-heading"><div><p className="eyebrow">LIVE CAPABILITIES</p><h3 id="tool-map-title">Tool routing map</h3></div><p><strong>{connectedCount}/{providers.length}</strong> providers active <span>·</span> <strong>{routeCount}</strong> routes</p></div>
    {providers.length === 0 ? <div className="tool-map-empty">Add a provider to see how public tools map upstream.</div> : <div className="tool-map-scroll"><table><thead><tr><th scope="col">Public tool</th>{providers.map(provider => <th scope="col" key={provider.id}><span className={provider.connected ? "provider-dot live" : "provider-dot"} />{provider.name}<small>{provider.kind}</small></th>)}</tr></thead><tbody>{canonicalTools.map(tool => <tr key={tool}><th scope="row"><code>{tool}</code></th>{providers.map(provider => { const mapping = provider.tool_mappings.find(item => item.canonical_tool === tool); return <td key={provider.id}>{mapping ? <div className="mapping"><code>{mapping.upstream_tool}</code><small>{mapping.upstream_tool === tool ? "direct" : "alias"}</small></div> : <span className="unmapped">{!provider.enabled ? "Disabled" : !provider.connected ? "Unavailable" : "Not supported"}</span>}</td>; })}</tr>)}</tbody></table></div>}
  </section>;
}

function Keys() {
  const [rows, setRows] = useState<ClientKey[]>([]); const [error, setError] = useState(""); const load = () => api<ClientKey[]>("/keys").then(setRows); useEffect(() => { void load(); }, []);
  async function create() { const name = prompt("Key name"); if (name) { try { await api<{ key: string }>("/keys", { method: "POST", body: JSON.stringify({ name }) }); setError(""); void load(); } catch { setError("Could not create key"); } } }
  async function copyKey(value: string) { try { if (!navigator.clipboard) throw new Error(); await navigator.clipboard.writeText(value); setError(""); } catch { setError("Copy failed. Select the key manually."); } }
  return <section><button onClick={create}>Create key</button>{error && <p className="error">{error}</p>}<div className="table">{rows.map(key => <div className="tr key" key={key.id}><span>{key.name}<small>{key.key ?? `${key.prefix}…`}</small></span><span>{key.status}</span><span>{key.request_count} calls</span><button className="secondary" disabled={!key.key} onClick={() => key.key && void copyKey(key.key)}>Copy</button></div>)}</div><p className="note">Active client API keys are shown in the list and can be copied again. Keys created before repeat-copy support must be replaced.</p></section>;
}

function Requests() {
  const [rows, setRows] = useState<RequestEvent[]>([]); useEffect(() => { void api<RequestEvent[]>("/requests").then(setRows); }, []);
  return <div className="table">{rows.map(row => <div className="tr request" key={row.request_id}><span>{row.request_id.slice(0, 8)}<small>{row.started_at}</small></span><span>{row.client_key_name ?? "—"}<small>{row.client_key_prefix ?? "—"}</small></span><span>{row.provider ?? "—"}</span><span>{row.duration_ms} ms</span><span className={row.outcome === "success" ? "ok" : "warn"}>{row.outcome}<small>{row.error_category ?? ""}</small></span></div>)}</div>;
}

function App() { const [authenticated, setAuthenticated] = useState<boolean | null>(null); useEffect(() => { void api("/session").then(() => setAuthenticated(true)).catch(() => setAuthenticated(false)); }, []); return authenticated === null ? null : authenticated ? <Panel signedOut={() => setAuthenticated(false)} /> : <Login done={() => setAuthenticated(true)} />; }
createRoot(document.getElementById("root")!).render(<App />);
