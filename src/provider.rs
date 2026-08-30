use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use rmcp::service::RunningService;
use rmcp::{
    ClientLifecycleMode, ClientServiceExt, RoleClient,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ClientInfo,
        PaginatedRequestParams, ProtocolVersion,
    },
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Map, Value};

use crate::{
    catalog::canonical_tools,
    db::{Database, ProviderConfig},
    router::WeightedRandom,
};

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("no connected provider currently supports {0}")]
    Unavailable(String),
    #[error("provider timed out")]
    Timeout,
    #[error("provider rate limited the request")]
    RateLimited(Option<u64>),
    #[error("provider session expired")]
    SessionExpired,
    #[error("provider request failed")]
    Transport,
    #[error("provider returned an error")]
    Upstream,
}

impl ProviderError {
    pub fn category(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "unavailable",
            Self::Timeout => "timeout",
            Self::RateLimited(_) => "rate_limited",
            Self::SessionExpired => "session_expired",
            Self::Transport => "transport",
            Self::Upstream => "upstream",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct ProviderFailure {
    pub provider_id: Option<i64>,
    pub error: ProviderError,
}

impl ProviderFailure {
    fn before_routing(error: ProviderError) -> Self {
        Self {
            provider_id: None,
            error,
        }
    }

    fn after_routing(provider_id: i64, error: ProviderError) -> Self {
        Self {
            provider_id: Some(provider_id),
            error,
        }
    }

    pub fn category(&self) -> &'static str {
        self.error.category()
    }
}

struct ConnectedProvider {
    id: i64,
    kind: String,
    endpoint: String,
    bearer_token: String,
    timeout_seconds: AtomicU64,
    upstream_tools: HashMap<String, String>,
    client: RunningService<RoleClient, ClientInfo>,
}

pub struct ProviderRegistry {
    router: RwLock<WeightedRandom<Arc<ConnectedProvider>>>,
}

impl ProviderRegistry {
    pub async fn new(db: Database) -> anyhow::Result<Self> {
        let registry = Self {
            router: RwLock::new(WeightedRandom::default()),
        };
        for provider in db.providers().await? {
            let description = format!("{} ({})", provider.name, provider.id);
            registry
                .add(provider)
                .await
                .with_context(|| format!("failed to connect provider {description}"))?;
        }
        Ok(registry)
    }

    /// Adds one provider to the live router without touching existing connections.
    pub async fn add(&self, provider: ProviderConfig) -> anyhow::Result<()> {
        if !provider.enabled {
            return Ok(());
        }
        if self
            .router
            .read()
            .expect("provider router poisoned")
            .find(|current| current.id == provider.id)
            .is_some()
        {
            anyhow::bail!("provider {} is already active", provider.id);
        }
        let id = provider.id;
        let weight = provider.weight;
        let (client, upstream_tools) = connect(&provider).await?;
        let connected = Arc::new(ConnectedProvider::new(provider, client, upstream_tools));
        let mut router = self.router.write().expect("provider router poisoned");
        if router.find(|current| current.id == id).is_some() {
            anyhow::bail!("provider {id} is already active");
        }
        router.upsert(connected, weight, |current| current.id == id);
        Ok(())
    }

    /// Applies one provider's configuration, reconnecting only when connection
    /// parameters changed or when a disabled provider becomes enabled.
    pub async fn update(&self, provider: ProviderConfig) -> anyhow::Result<()> {
        let previous = self
            .router
            .read()
            .expect("provider router poisoned")
            .find(|current| current.id == provider.id)
            .cloned();

        if !provider.enabled {
            self.router
                .write()
                .expect("provider router poisoned")
                .remove(|current| current.id == provider.id);
            return Ok(());
        }

        let id = provider.id;
        let weight = provider.weight;
        let connected = match previous.as_ref() {
            Some(current) if current.has_same_connection(&provider) => {
                current
                    .timeout_seconds
                    .store(provider.timeout_seconds as u64, Ordering::Relaxed);
                Arc::clone(current)
            }
            _ => {
                let (client, upstream_tools) = connect(&provider).await?;
                Arc::new(ConnectedProvider::new(provider, client, upstream_tools))
            }
        };
        self.router
            .write()
            .expect("provider router poisoned")
            .upsert(connected, weight, |current| current.id == id);
        Ok(())
    }

    /// Removes one provider from routing and closes only its connection.
    pub fn remove(&self, id: i64) {
        self.router
            .write()
            .expect("provider router poisoned")
            .remove(|provider| provider.id == id);
    }

    pub fn has_providers(&self) -> bool {
        !self
            .router
            .read()
            .expect("provider router poisoned")
            .is_empty()
    }

    pub async fn call(
        &self,
        tool_name: &str,
        arguments: Map<String, Value>,
    ) -> Result<(i64, CallToolResult), ProviderFailure> {
        let provider = self
            .router
            .read()
            .expect("provider router poisoned")
            .next_provider_matching(|provider| provider.upstream_tools.contains_key(tool_name))
            .cloned()
            .ok_or_else(|| {
                ProviderFailure::before_routing(ProviderError::Unavailable(tool_name.to_owned()))
            })?;
        let id = provider.id;
        let upstream_name = provider
            .upstream_tools
            .get(tool_name)
            .expect("selected provider supports requested tool")
            .clone();
        let params = CallToolRequestParams::new(upstream_name).with_arguments(arguments);
        let outcome = tokio::time::timeout(
            Duration::from_secs(provider.timeout_seconds.load(Ordering::Relaxed)),
            provider.client.call_tool_once(params),
        )
        .await;
        match outcome {
            Err(_) => Err(ProviderFailure::after_routing(id, ProviderError::Timeout)),
            Ok(Ok(CallToolResponse::Complete(result))) => Ok((id, result)),
            Ok(Ok(_)) => Err(ProviderFailure::after_routing(id, ProviderError::Upstream)),
            Ok(Err(error)) => {
                let text = error.to_string().to_ascii_lowercase();
                let returned = if text.contains("session expired") {
                    ProviderError::SessionExpired
                } else if text.contains("429") || text.contains("rate limit") {
                    ProviderError::RateLimited(None)
                } else {
                    ProviderError::Transport
                };
                Err(ProviderFailure::after_routing(id, returned))
            }
        }
    }
}

impl ConnectedProvider {
    fn new(
        provider: ProviderConfig,
        client: RunningService<RoleClient, ClientInfo>,
        upstream_tools: HashMap<String, String>,
    ) -> Self {
        Self {
            id: provider.id,
            kind: provider.kind,
            endpoint: provider.endpoint,
            bearer_token: provider.bearer_token,
            timeout_seconds: AtomicU64::new(provider.timeout_seconds as u64),
            upstream_tools,
            client,
        }
    }

    fn has_same_connection(&self, provider: &ProviderConfig) -> bool {
        self.kind == provider.kind
            && self.endpoint == provider.endpoint
            && self.bearer_token == provider.bearer_token
    }
}

async fn connect(
    provider: &ProviderConfig,
) -> anyhow::Result<(
    RunningService<RoleClient, ClientInfo>,
    HashMap<String, String>,
)> {
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(provider.endpoint.clone())
            .auth_header(provider.bearer_token.clone())
            // An in-flight tool call must never be replayed after session recovery.
            .reinit_on_expired_session(false),
    );
    let mut client = tokio::time::timeout(
        Duration::from_secs(provider.timeout_seconds as u64),
        ClientInfo::default().serve_with_lifecycle(
            transport,
            ClientLifecycleMode::Auto {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                legacy_version: Some(ProtocolVersion::V_2025_11_25),
            },
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("initialize timed out"))??;
    let catalog = async {
        let mut cursor = None;
        let mut upstream_names = HashSet::new();
        loop {
            let params = cursor
                .clone()
                .map(|value| PaginatedRequestParams::default().with_cursor(Some(value)));
            let page = tokio::time::timeout(
                Duration::from_secs(provider.timeout_seconds as u64),
                client.list_tools(params),
            )
            .await
            .map_err(|_| anyhow::anyhow!("tools/list timed out"))??;
            for tool in page.tools {
                upstream_names.insert(tool.name.into_owned());
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }

        let mapped = canonical_tools()
            .iter()
            .filter_map(|tool| {
                mapped_name_candidates(&provider.kind, tool.name.as_ref())
                    .into_iter()
                    .find(|upstream_name| upstream_names.contains(upstream_name.as_ref()))
                    .map(|upstream_name| (tool.name.to_string(), upstream_name.into_owned()))
            })
            .collect::<HashMap<_, _>>();
        if mapped.is_empty() {
            anyhow::bail!("mapped Tavily tools are absent");
        }
        Ok(mapped)
    }
    .await;
    match catalog {
        Ok(mapped) => Ok((client, mapped)),
        Err(error) => {
            let _ = client.close().await;
            Err(error)
        }
    }
}

pub fn mapped_name_candidates<'a>(kind: &str, canonical_name: &'a str) -> Vec<Cow<'a, str>> {
    if kind == "searchix" {
        vec![
            Cow::Owned(format!("search_proxy_{canonical_name}")),
            Cow::Borrowed(canonical_name),
        ]
    } else {
        vec![Cow::Borrowed(canonical_name)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_name_candidates_cover_every_canonical_tool() {
        for tool in canonical_tools() {
            assert_eq!(
                mapped_name_candidates("searchix", tool.name.as_ref()),
                [format!("search_proxy_{}", tool.name), tool.name.to_string()]
            );
            assert_eq!(
                mapped_name_candidates("tavily_hikari", tool.name.as_ref()),
                [tool.name.to_string()]
            );
        }
    }
}
