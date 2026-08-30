# Tavily official five-tool catalog and schema compatibility

Research date: 2026-08-30

## Conclusion and exact tool-name mapping

Tavily's official MCP source at the pinned commit exposes exactly five tools:[^mcp-catalog]

- `tavily_search`
- `tavily_extract`
- `tavily_crawl`
- `tavily_map`
- `tavily_research`

Tavily Hikari's normal/default proxy mode preserves that canonical five-name catalog from its configured Tavily upstream. For Searchix, this repository verifies only `search_proxy_tavily_search`; no inspected Searchix primary source or authenticated `tools/list` result verified `search_proxy_tavily_extract`, `search_proxy_tavily_crawl`, `search_proxy_tavily_map`, or `search_proxy_tavily_research`. Those four prefixed names must therefore be treated as **unverified**, not inferred from the search naming pattern.

| Canonical public tool | Tavily official | Tavily Hikari normal mode | Searchix |
|---|---|---|---|
| Search | `tavily_search` | `tavily_search` | `search_proxy_tavily_search` (verified by current adapter) |
| Extract | `tavily_extract` | `tavily_extract` | Unverified; do not assume `search_proxy_tavily_extract` |
| Crawl | `tavily_crawl` | `tavily_crawl` | Unverified; do not assume `search_proxy_tavily_crawl` |
| Map | `tavily_map` | `tavily_map` | Unverified; do not assume `search_proxy_tavily_map` |
| Research | `tavily_research` | `tavily_research` | Unverified; do not assume `search_proxy_tavily_research` |

The implemented gateway pins this complete official MCP catalog as its stable public contract. During provider activation it paginates the upstream `tools/list`, records each matched tool's exact upstream name, and routes a call only among providers that advertised that capability. The public catalog remains stable even when no provider currently supports a tool; such a call returns an unavailable result instead of being sent to an incompatible provider.

This allows all five tools through Hikari while keeping Searchix limited to capabilities actually observed at connection time. Searchix discovery checks both the canonical name and its `search_proxy_` candidate, but the presence of a candidate is never assumed. The remaining compatibility gap is semantic: upstream `inputSchema` values are not yet compared with the pinned schema.

## Exact MCP comparison

The official source at commit `248dc9e3e385305ad3281120284ff662af4b5940` defines the tool and schema directly in its `tools/list` handler.[^mcp-source] Before the pinned catalog was introduced, the gateway used a hand-written search-only schema.

| Field | Previous hand-written gateway schema | Official Tavily MCP | Compatibility effect of switching |
|---|---|---|---|
| `query` | `string`; required | `string`; required; description | Same validation |
| `country` | `string` | `string`; default `""`; full-name guidance | Same type; adds an annotation/default |
| `end_date` | `string` | `string`; default `""`; documented `YYYY-MM-DD` | Same validation; official schema has no `format` or pattern constraint |
| `exact_match` | `boolean` | `boolean`; description; no default | Same validation |
| `exclude_domains` | array of `string` | array of `string`; default `[]` | Same validation |
| `include_domains` | array of `string` | array of `string`; default `[]` | Same validation |
| `include_favicon` | `boolean` | `boolean`; default `false` | Same validation |
| `include_image_descriptions` | `boolean` | `boolean`; default `false` | Same validation |
| `include_images` | `boolean` | `boolean`; default `false` | Same validation |
| `include_raw_content` | `boolean` | `boolean`; default `false` | Same validation |
| `max_results` | `integer`, minimum 1, maximum 20 | `number`, default 5, minimum 5, maximum 20 | Breaking for current values 1-4; official schema also admits fractions that the REST contract does not |
| `search_depth` | unconstrained `string` | `string`; enum `basic`, `advanced`, `fast`, `ultra-fast`; default `basic` | Tightens accepted values |
| `start_date` | `string` | `string`; default `""`; documented `YYYY-MM-DD` | Same validation; official schema has no `format` or pattern constraint |
| `time_range` | unconstrained `string` | `string`; enum `day`, `week`, `month`, `year`; no default | Tightens accepted values |
| `topic` | unconstrained `string` | `string`; enum containing only `general`; default `general` | Would hide/reject `news` and `finance`, although REST supports them |

Root-level differences:

- Both require only `query`.
- The previous schema set `additionalProperties: false`; the official MCP source omits `additionalProperties`, which means additional properties are allowed by JSON Schema. Nevertheless, Tavily's MCP call handler reconstructs the REST payload from the 15 named arguments, so unlisted arguments are not forwarded by that implementation.[^mcp-call]
- The previous schema omitted all Tavily descriptions and schema `default` annotations. JSON Schema `default` is normally an annotation, not proof that this gateway or either upstream will insert the value.

## MCP schema is not the REST schema

Tavily's current REST OpenAPI request has 22 fields, while the official MCP tool exposes only the 15 above.[^rest]

The REST-only fields are `chunks_per_source`, `include_answer`, `language`, `filter_by_language`, `auto_parameters`, `include_usage`, and `safe_search`. Important shared-field differences are:

- REST `max_results`: `integer`, default 5, range 0-20. MCP: `number`, default 5, range 5-20.
- REST `topic`: `general | news | finance`. MCP: only `general`.
- REST `time_range`: full names plus `d | w | m | y`, default `null`. MCP: full names only and no default.
- REST `include_raw_content`: boolean or `markdown | text`. MCP: boolean only.
- REST `country`: a fixed lowercase country enum, default `null`. MCP: unconstrained string, default `""`, with prose requiring a full country name.
- REST dates default to `null`; MCP dates default to `""`. Both document `YYYY-MM-DD` but neither schema adds a date-format/pattern constraint.

Consequently, copying the REST request schema into the pinned catalog would not be “using Tavily's official MCP schema”; it would expose arguments the official MCP implementation does not accept/forward and would change several types and enums.

## Provider-specific risk

### Tavily Hikari

Hikari describes itself as a proxy for Tavily's MCP endpoint and defaults its upstream to `https://mcp.tavily.com/mcp`.[^hikari-readme] Its normal proxy code clones the incoming body as `forwarded_body` and sends that body upstream.[^hikari-forward] Therefore, the official MCP field set is the natural schema for this provider.

Risks remain:

- Hikari tracks the schema exposed by its configured upstream, not necessarily the public GitHub commit inspected here; a custom or older `TAVILY_UPSTREAM` can differ.
- The gateway now records all matching tool names during `tools/list`, but still discards the returned schemas. Schema drift is therefore invisible at activation.
- Adopting the official MCP schema's `number` type can encourage fractional `max_results`, which the REST API documents as invalid.

### Searchix

The repository maps advertised Searchix tool names to the canonical catalog and forwards arguments unchanged. No Searchix schema or endpoint credential is checked into the repository. Activation verifies tool names but does not compare `inputSchema`.

Until a real authenticated Searchix `tools/list` result is captured and compared, semantic compatibility is unproven. Searchix may accept different `max_results` bounds or provider-specific `search_depth`, `time_range`, and `topic` values, and it may not implement Tavily's documented defaults. The gateway advertises the pinned official schema but does not insert defaults or validate arguments before forwarding them.

Capability-aware routing prevents calls from reaching a Searchix connection that did not advertise the selected tool. Full schema compatibility still requires an authenticated Searchix `tools/list` comparison or explicit argument normalization.

## Primary sources

[^mcp-catalog]: Tavily official MCP complete `tools/list` catalog, including descriptions and input schemas for all five tools, lines 165-444: <https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L165-L444>
[^mcp-source]: Tavily official MCP source, `tavily_search.inputSchema`, lines 165-256: <https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L165-L256>
[^mcp-call]: Tavily official MCP call handler reconstructs the search request from the 15 named arguments, lines 446-474: <https://github.com/tavily-ai/tavily-mcp/blob/248dc9e3e385305ad3281120284ff662af4b5940/src/index.ts#L446-L474>
[^rest]: Tavily official Search REST OpenAPI: <https://docs.tavily.com/documentation/api-reference/endpoint/search.md>
[^hikari-readme]: Tavily Hikari project README and upstream configuration: <https://github.com/IvanLi-CN/tavily-hikari/blob/c65f8a405d18ad1e2f53356098eae16a6597da13/README.md>
[^hikari-forward]: Tavily Hikari normal proxy path preserves and forwards the request body: <https://github.com/IvanLi-CN/tavily-hikari/blob/c65f8a405d18ad1e2f53356098eae16a6597da13/src/server/proxy/proxy_handlers_and_views.rs#L490-L520> and <https://github.com/IvanLi-CN/tavily-hikari/blob/c65f8a405d18ad1e2f53356098eae16a6597da13/src/server/proxy/proxy_handlers_and_views.rs#L1085-L1172>

Tavily's official MCP documentation identifies both the remote endpoint and the official GitHub implementation: <https://docs.tavily.com/documentation/mcp.md>. An unauthenticated live `tools/list` could not be used as a source because the remote endpoint requires OAuth or an API key.
