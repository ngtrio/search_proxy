use std::{
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
use serde_json::{Map, Value, json};

use crate::{
    db::{Database, ProviderRow},
    router::WeightedRandom,
};

const CANONICAL_FIELDS: &[&str] = &[
    "query",
    "country",
    "end_date",
    "exact_match",
    "exclude_domains",
    "include_domains",
    "include_favicon",
    "include_image_descriptions",
    "include_images",
    "include_raw_content",
    "max_results",
    "search_depth",
    "start_date",
    "time_range",
    "topic",
];

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("no connected provider is currently available")]
    Unavailable,
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
            Self::Unavailable => "unavailable",
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
    client: RunningService<RoleClient, ClientInfo>,
}

pub struct ProviderRegistry {
    router: RwLock<WeightedRandom<Arc<ConnectedProvider>>>,
}

impl ProviderRegistry {
    pub async fn new(db: Database) -> anyhow::Result<Self> {
        let providers = db
            .providers()
            .await?
            .into_iter()
            .filter(|provider| provider.enabled)
            .collect::<Vec<_>>();
        let mut connected = Vec::with_capacity(providers.len());
        for provider in providers {
            match connect(&provider).await {
                Ok(client) => {
                    let weight = provider.weight;
                    connected.push((connected_provider(provider, client), weight));
                }
                Err(error) => {
                    drop(connected);
                    return Err(error).with_context(|| {
                        format!(
                            "failed to connect provider {} ({})",
                            provider.name, provider.id
                        )
                    });
                }
            }
        }
        let mut router = WeightedRandom::default();
        router.replace(connected);
        Ok(Self {
            router: RwLock::new(router),
        })
    }

    /// Adds one provider to the live router without touching existing connections.
    pub async fn add(&self, provider: ProviderRow) -> anyhow::Result<()> {
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
        let client = connect(&provider).await?;
        let connected = connected_provider(provider, client);
        let mut router = self.router.write().expect("provider router poisoned");
        if router.find(|current| current.id == id).is_some() {
            anyhow::bail!("provider {id} is already active");
        }
        router.upsert(connected, weight, |current| current.id == id);
        Ok(())
    }

    /// Applies one provider's configuration, reconnecting only when connection
    /// parameters changed or when a disabled provider becomes enabled.
    pub async fn update(&self, provider: ProviderRow) -> anyhow::Result<()> {
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
                let client = connect(&provider).await?;
                connected_provider(provider, client)
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
        arguments: Map<String, Value>,
    ) -> Result<(i64, CallToolResult), ProviderFailure> {
        let provider = self
            .router
            .read()
            .expect("provider router poisoned")
            .next_provider()
            .cloned()
            .ok_or_else(|| ProviderFailure::before_routing(ProviderError::Unavailable))?;
        let id = provider.id;
        let params =
            CallToolRequestParams::new(mapped_name(&provider.kind)).with_arguments(arguments);
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
    fn has_same_connection(&self, provider: &ProviderRow) -> bool {
        self.kind == provider.kind
            && self.endpoint == provider.endpoint
            && self.bearer_token == provider.bearer_token
    }
}

fn connected_provider(
    provider: ProviderRow,
    client: RunningService<RoleClient, ClientInfo>,
) -> Arc<ConnectedProvider> {
    Arc::new(ConnectedProvider {
        id: provider.id,
        kind: provider.kind,
        endpoint: provider.endpoint,
        bearer_token: provider.bearer_token,
        timeout_seconds: AtomicU64::new(provider.timeout_seconds as u64),
        client,
    })
}

async fn connect(provider: &ProviderRow) -> anyhow::Result<RunningService<RoleClient, ClientInfo>> {
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
            if page
                .tools
                .iter()
                .any(|tool| tool.name == mapped_name(&provider.kind))
            {
                return Ok(());
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                anyhow::bail!("mapped search tool is absent");
            }
        }
    }
    .await;
    if let Err(error) = catalog {
        let _ = client.close().await;
        return Err(error);
    }
    Ok(client)
}

pub fn mapped_name(kind: &str) -> &'static str {
    if kind == "searchix" {
        "search_proxy_tavily_search"
    } else {
        "tavily_search"
    }
}

pub fn canonical_schema() -> Map<String, Value> {
    let mut properties = Map::new();
    properties.insert("query".into(), json!({"type":"string"}));
    for field in CANONICAL_FIELDS.iter().skip(1) {
        let schema = if ["exclude_domains", "include_domains"].contains(field) {
            json!({"type":"array","items":{"type":"string"}})
        } else if *field == "max_results" {
            json!({"type":"integer","minimum":1,"maximum":20})
        } else if field.starts_with("include_") || *field == "exact_match" {
            json!({"type":"boolean"})
        } else {
            json!({"type":"string"})
        };
        properties.insert((*field).into(), schema);
    }
    json!({"type":"object","properties":properties,"required":["query"],"additionalProperties":false}).as_object().expect("schema object").clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_names_are_internal() {
        assert_eq!(mapped_name("searchix"), "search_proxy_tavily_search");
        assert_eq!(mapped_name("tavily_hikari"), "tavily_search");
    }
}
