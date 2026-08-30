import {
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import {
  Link,
  Outlet,
  useNavigate,
  useRouterState,
} from "@tanstack/react-router";
import { useState } from "react";
import type { FormEvent } from "react";
import { api, canonicalTools, ProviderKind, setCsrf } from "./api";
import type { ClientKey, OverviewData, Provider, RequestEvent } from "./api";

type ProviderForm = Pick<Provider, "kind" | "name" | "endpoint" | "weight" | "enabled"> & {
  token: string;
};

type Session = { authenticated: boolean };
type LoginInput = { username: string; password: string };

const sessionQueryKey = ["session"] as const;
const emptyProvider = (): ProviderForm => ({
  kind: ProviderKind.Searchix,
  name: "",
  endpoint: "https://",
  token: "",
  weight: 1,
  enabled: true,
});

function LoadingScreen() {
  return (
    <main className="loading">
      <p className="eyebrow">CONTROL PLANE</p>
      <p>Checking session…</p>
    </main>
  );
}

function Login() {
  const navigate = useNavigate();
  const client = useQueryClient();
  const loginMutation = useMutation({
    mutationFn: (input: LoginInput) =>
      api<{ csrf_token: string }>("/login", {
        method: "POST",
        body: JSON.stringify(input),
      }),
    onSuccess: (result) => {
      setCsrf(result.csrf_token);
      client.setQueryData<Session>(sessionQueryKey, { authenticated: true });
      void navigate({ to: "/" });
    },
  });

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const data = new FormData(event.currentTarget);
    loginMutation.mutate({
      username: String(data.get("username") ?? ""),
      password: String(data.get("password") ?? ""),
    });
  }

  return (
    <main className="login">
      <form onSubmit={submit}>
        <p className="eyebrow">CONTROL PLANE</p>
        <h1>Tavily Gateway</h1>
        <label>
          Username
          <input name="username" autoComplete="username" required />
        </label>
        <label>
          Password
          <input name="password" type="password" autoComplete="current-password" required />
        </label>
        {loginMutation.isError && <p className="error">Invalid credentials</p>}
        <button disabled={loginMutation.isPending}>
          {loginMutation.isPending ? "Signing in…" : "Sign in"}
        </button>
      </form>
    </main>
  );
}

export function RootLayout() {
  const sessionQuery = useQuery<Session>({
    queryKey: sessionQueryKey,
    queryFn: () => api<Session>("/session"),
    retry: false,
  });

  if (sessionQuery.isPending) return <LoadingScreen />;
  if (sessionQuery.isError || !sessionQuery.data?.authenticated) return <Login />;
  return <Shell />;
}

function Shell() {
  const client = useQueryClient();
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const logoutMutation = useMutation({
    mutationFn: () => api<void>("/logout", { method: "POST", body: "{}" }),
    onSuccess: () => {
      setCsrf("");
      client.removeQueries({ predicate: (query) => query.queryKey[0] !== sessionQueryKey[0] });
      client.setQueryData<Session>(sessionQueryKey, { authenticated: false });
    },
  });

  const titles: Record<string, string> = {
    "/": "Overview",
    "/providers": "Providers",
    "/keys": "Keys",
    "/requests": "Requests",
  };

  return (
    <div className="shell">
      <aside>
        <div>
          <p className="eyebrow">TAVILY</p>
          <h2>Gateway</h2>
        </div>
        <nav aria-label="Primary navigation">
          <Link to="/" activeOptions={{ exact: true }} activeProps={{ className: "active" }}>
            Overview
          </Link>
          <Link to="/providers" activeProps={{ className: "active" }}>
            Providers
          </Link>
          <Link to="/keys" activeProps={{ className: "active" }}>
            Keys
          </Link>
          <Link to="/requests" activeProps={{ className: "active" }}>
            Requests
          </Link>
        </nav>
        <button
          className="signout"
          disabled={logoutMutation.isPending}
          onClick={() => logoutMutation.mutate()}
        >
          {logoutMutation.isPending ? "Signing out…" : "Sign out"}
        </button>
        {logoutMutation.isError && <small className="error">Could not sign out</small>}
        <small>Metadata only · no query retention</small>
      </aside>
      <main>
        <header>
          <p className="eyebrow">OPERATIONS</p>
          <h1>{titles[pathname] ?? "Operations"}</h1>
        </header>
        <Outlet />
      </main>
    </div>
  );
}

function QueryLoading() {
  return <p className="status">Loading…</p>;
}

function QueryError() {
  return <p className="error">Could not load this data. Try again shortly.</p>;
}

export function Overview() {
  const query = useQuery({
    queryKey: ["overview"],
    queryFn: () => api<OverviewData>("/overview"),
  });

  if (query.isPending) return <QueryLoading />;
  if (query.isError) return <QueryError />;

  const data = query.data;
  const totals = {
    requests: data.requests,
    successes: data.successes,
    failures: data.failures,
    average_latency_ms: data.average_latency_ms,
  };

  return (
    <section>
      <div className="cards">
        {Object.entries(totals).map(([key, value]) => (
          <article key={key}>
            <span>{key.replaceAll("_", " ")}</span>
            <strong>{value}</strong>
          </article>
        ))}
      </div>
      <h3>Provider performance</h3>
      <div className="table">
        <div className="tr metric head">
          <span>Provider</span>
          <span>Requests</span>
          <span>Success</span>
          <span>Failure</span>
          <span>Avg latency</span>
        </div>
        {data.providers.map((row) => (
          <div className="tr metric" key={row.provider}>
            <span>{row.provider}</span>
            <span>{row.requests}</span>
            <span className="ok">{row.successes}</span>
            <span className="warn">{row.failures}</span>
            <span>{row.average_latency_ms} ms</span>
          </div>
        ))}
      </div>
      <h3>30-day latency trend</h3>
      <div className="table">
        <div className="tr metric head">
          <span>Day</span>
          <span>Requests</span>
          <span>Success</span>
          <span>Failure</span>
          <span>Avg latency</span>
        </div>
        {data.daily.map((row) => (
          <div className="tr metric" key={row.day}>
            <span>{row.day}</span>
            <span>{row.requests}</span>
            <span className="ok">{row.successes}</span>
            <span className="warn">{row.failures}</span>
            <span>{row.average_latency_ms} ms</span>
          </div>
        ))}
      </div>
    </section>
  );
}

export function Providers() {
  const client = useQueryClient();
  const [editing, setEditing] = useState<number | null>(null);
  const [form, setForm] = useState<ProviderForm>(emptyProvider());
  const providersQuery = useQuery({
    queryKey: ["providers"],
    queryFn: () => api<Provider[]>("/providers"),
  });
  const saveMutation = useMutation({
    mutationFn: ({ id, values }: { id: number | null; values: ProviderForm }) =>
      api<void>(id ? `/providers/${id}` : "/providers", {
        method: id ? "PUT" : "POST",
        body: JSON.stringify(values),
      }),
    onSuccess: () => {
      setEditing(null);
      void client.invalidateQueries({ queryKey: ["providers"] });
    },
  });

  function edit(provider?: Provider) {
    setEditing(provider?.id ?? 0);
    setForm(
      provider
        ? {
            kind: provider.kind,
            name: provider.name,
            endpoint: provider.endpoint,
            token: "",
            weight: provider.weight,
            enabled: provider.enabled,
          }
        : emptyProvider(),
    );
  }

  function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    saveMutation.mutate({ id: editing, values: form });
  }

  if (providersQuery.isPending) return <QueryLoading />;
  if (providersQuery.isError) return <QueryError />;
  const rows = providersQuery.data;

  return (
    <section>
      <button onClick={() => edit()}>Add provider</button>
      {editing !== null && (
        <form className="editor" onSubmit={save}>
          <label>
            Name
            <input
              value={form.name}
              onChange={(event) => setForm((current) => ({ ...current, name: event.target.value }))}
              required
            />
          </label>
          <label>
            Adapter
            <select
              value={form.kind}
              onChange={(event) =>
                setForm((current) => ({ ...current, kind: event.target.value as ProviderKind }))
              }
            >
              <option value={ProviderKind.Searchix}>Searchix</option>
              <option value={ProviderKind.TavilyHikari}>Tavily Hikari</option>
            </select>
          </label>
          <label>
            Endpoint
            <input
              type="url"
              value={form.endpoint}
              onChange={(event) => setForm((current) => ({ ...current, endpoint: event.target.value }))}
              required
            />
          </label>
          <label>
            Token (write-only)
            <input
              type="password"
              value={form.token}
              onChange={(event) => setForm((current) => ({ ...current, token: event.target.value }))}
              required={editing === 0}
              placeholder={editing ? "Leave blank to retain" : "Required"}
            />
          </label>
          <label>
            Weight
            <input
              type="number"
              min="1"
              value={form.weight}
              onChange={(event) =>
                setForm((current) => ({ ...current, weight: Number(event.target.value) }))
              }
            />
          </label>
          <label className="check">
            <input
              type="checkbox"
              checked={form.enabled}
              onChange={(event) =>
                setForm((current) => ({ ...current, enabled: event.target.checked }))
              }
            />
            Enabled
          </label>
          {saveMutation.isError && <p className="error">Could not save provider</p>}
          <div>
            <button disabled={saveMutation.isPending}>
              {saveMutation.isPending ? "Saving…" : "Save"}
            </button>{" "}
            <button type="button" className="secondary" onClick={() => setEditing(null)}>
              Cancel
            </button>
          </div>
        </form>
      )}
      <div className="table">
        <div className="tr head">
          <span>Name</span>
          <span>Adapter</span>
          <span>Status</span>
          <span>Weight</span>
          <span />
        </div>
        {rows.map((provider) => {
          const status = !provider.enabled
            ? "disabled"
            : provider.connected
              ? "connected"
              : "unavailable";
          return (
            <div className="tr" key={provider.id}>
              <span>
                {provider.name}
                <small>{provider.endpoint}</small>
              </span>
              <span>{provider.kind}</span>
              <span className={provider.connected ? "ok" : "warn"}>{status}</span>
              <span>{provider.weight}</span>
              <span>
                <button className="secondary" onClick={() => edit(provider)}>
                  Edit
                </button>
              </span>
            </div>
          );
        })}
      </div>
      <ToolMappingMatrix providers={rows} />
      <p className="note">
        Mappings come from each connected provider&apos;s live tools/list response. Provider tokens are
        write-only.
      </p>
    </section>
  );
}

function ToolMappingMatrix({ providers }: { providers: Provider[] }) {
  const routeCount = providers.reduce((total, provider) => total + provider.tool_mappings.length, 0);
  const connectedCount = providers.filter((provider) => provider.connected).length;

  return (
    <section className="tool-map" aria-labelledby="tool-map-title">
      <div className="tool-map-heading">
        <div>
          <p className="eyebrow">LIVE CAPABILITIES</p>
          <h3 id="tool-map-title">Tool routing map</h3>
        </div>
        <p>
          <strong>
            {connectedCount}/{providers.length}
          </strong>{" "}
          providers active <span>·</span> <strong>{routeCount}</strong> routes
        </p>
      </div>
      {providers.length === 0 ? (
        <div className="tool-map-empty">Add a provider to see how public tools map upstream.</div>
      ) : (
        <div className="tool-map-scroll">
          <table>
            <thead>
              <tr>
                <th scope="col">Public tool</th>
                {providers.map((provider) => (
                  <th scope="col" key={provider.id}>
                    <span className={provider.connected ? "provider-dot live" : "provider-dot"} />
                    {provider.name}
                    <small>{provider.kind}</small>
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {canonicalTools.map((tool) => (
                <tr key={tool}>
                  <th scope="row">
                    <code>{tool}</code>
                  </th>
                  {providers.map((provider) => {
                    const mapping = provider.tool_mappings.find((item) => item.canonical_tool === tool);
                    return (
                      <td key={provider.id}>
                        {mapping ? (
                          <div className="mapping">
                            <code>{mapping.upstream_tool}</code>
                            <small>{mapping.upstream_tool === tool ? "direct" : "alias"}</small>
                          </div>
                        ) : (
                          <span className="unmapped">
                            {!provider.enabled
                              ? "Disabled"
                              : !provider.connected
                                ? "Unavailable"
                                : "Not supported"}
                          </span>
                        )}
                      </td>
                    );
                  })}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

export function Keys() {
  const client = useQueryClient();
  const [copyError, setCopyError] = useState("");
  const keysQuery = useQuery({
    queryKey: ["keys"],
    queryFn: () => api<ClientKey[]>("/keys"),
  });
  const createKeyMutation = useMutation({
    mutationFn: (name: string) =>
      api<{ key: string }>("/keys", {
        method: "POST",
        body: JSON.stringify({ name }),
      }),
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: ["keys"] });
    },
  });

  function create() {
    const name = prompt("Key name");
    if (name) {
      setCopyError("");
      createKeyMutation.mutate(name);
    }
  }

  async function copyKey(value: string) {
    try {
      if (!navigator.clipboard) throw new Error("clipboard unavailable");
      await navigator.clipboard.writeText(value);
      setCopyError("");
    } catch {
      setCopyError("Copy failed. Select the key manually.");
    }
  }

  if (keysQuery.isPending) return <QueryLoading />;
  if (keysQuery.isError) return <QueryError />;
  const rows = keysQuery.data;

  return (
    <section>
      <button disabled={createKeyMutation.isPending} onClick={create}>
        {createKeyMutation.isPending ? "Creating…" : "Create key"}
      </button>
      {createKeyMutation.isError && <p className="error">Could not create key</p>}
      {copyError && <p className="error">{copyError}</p>}
      <div className="table">
        {rows.map((key) => (
          <div className="tr key" key={key.id}>
            <span>
              {key.name}
              <small>{key.key ?? `${key.prefix}…`}</small>
            </span>
            <span>{key.status}</span>
            <span>{key.request_count} calls</span>
            <button
              className="secondary"
              disabled={!key.key}
              onClick={() => key.key && void copyKey(key.key)}
            >
              Copy
            </button>
          </div>
        ))}
      </div>
      <p className="note">
        Active client API keys are shown in the list and can be copied again. Keys created before
        repeat-copy support must be replaced.
      </p>
    </section>
  );
}

export function Requests() {
  const query = useQuery({
    queryKey: ["requests"],
    queryFn: () => api<RequestEvent[]>("/requests"),
  });

  if (query.isPending) return <QueryLoading />;
  if (query.isError) return <QueryError />;

  return (
    <div className="table">
      {query.data.map((row) => (
        <div className="tr request" key={row.request_id}>
          <span>
            {row.request_id.slice(0, 8)}
            <small>{row.started_at}</small>
          </span>
          <span>
            {row.client_key_name ?? "—"}
            <small>{row.client_key_prefix ?? "—"}</small>
          </span>
          <span>{row.provider ?? "—"}</span>
          <span>{row.duration_ms} ms</span>
          <span className={row.outcome === "success" ? "ok" : "warn"}>
            {row.outcome}
            <small>{row.error_category ?? ""}</small>
          </span>
        </div>
      ))}
    </div>
  );
}
