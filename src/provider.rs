use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

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
    db::{Database, NewProvider, ProviderConfig, ProviderUpdate},
    router::WeightedRandom,
};

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("no connected provider currently supports {0}")]
    Unavailable(String),
    #[error("provider timed out")]
    Timeout,
    #[error("provider rate limited the request")]
    RateLimited,
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
            Self::RateLimited => "rate_limited",
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

#[derive(Debug, thiserror::Error)]
pub enum ProviderMutationError {
    #[error("invalid provider configuration: {0}")]
    Invalid(&'static str),
    #[error("provider connection validation failed")]
    Connection(#[source] anyhow::Error),
    #[error("provider persistence failed")]
    Storage(#[from] sqlx::Error),
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
    weight: i64,
    kind: String,
    endpoint: String,
    bearer_token: String,
    timeout_seconds: AtomicU64,
    reconnect_required: AtomicBool,
    upstream_tools: HashMap<String, String>,
    client: RunningService<RoleClient, ClientInfo>,
}

pub struct ProviderManager {
    db: Database,
    router: RwLock<WeightedRandom<Arc<ConnectedProvider>>>,
    reconnect_lock: tokio::sync::Mutex<()>,
}

impl ProviderManager {
    pub async fn new(db: Database) -> anyhow::Result<Self> {
        let registry = Self {
            db: db.clone(),
            router: RwLock::new(WeightedRandom::default()),
            reconnect_lock: tokio::sync::Mutex::new(()),
        };
        for provider in db.providers().await? {
            let description = format!("{} ({})", provider.name, provider.id);
            if let Err(error) = registry.activate(provider).await {
                tracing::warn!(%error, provider = description, "provider unavailable during startup");
            }
        }
        Ok(registry)
    }

    /// Validates an enabled provider before persisting it, then installs the
    /// already-connected client into the live router.
    pub async fn create(&self, provider: NewProvider) -> Result<i64, ProviderMutationError> {
        validate_provider(
            &provider.kind,
            &provider.name,
            &provider.endpoint,
            &provider.bearer_token,
            provider.weight,
            provider.timeout_seconds,
        )?;
        let prepared = if provider.enabled {
            Some(
                connect(
                    &provider.kind,
                    &provider.endpoint,
                    &provider.bearer_token,
                    provider.timeout_seconds,
                    canonical_tools(),
                )
                .await
                .map_err(ProviderMutationError::Connection)?,
            )
        } else {
            None
        };

        let id = match self.db.create_provider(provider.clone()).await {
            Ok(id) => id,
            Err(error) => {
                if let Some((mut client, _)) = prepared {
                    let _ = client.close().await;
                }
                return Err(ProviderMutationError::Storage(error));
            }
        };
        if let Some((client, upstream_tools)) = prepared {
            self.install(ConnectedProvider::from_new(
                id,
                provider,
                client,
                upstream_tools,
            ));
        }
        Ok(id)
    }

    /// Validates connection-changing updates before persistence. Once the
    /// database write succeeds, applying the prepared runtime state cannot fail.
    pub async fn update_config(
        &self,
        id: i64,
        update: ProviderUpdate,
    ) -> Result<bool, ProviderMutationError> {
        let Some(current_config) = self.db.provider(id).await? else {
            return Ok(false);
        };
        let desired = ProviderConfig {
            id,
            kind: update.kind.clone(),
            name: update.name.clone(),
            endpoint: update.endpoint.clone(),
            bearer_token: update
                .bearer_token
                .as_deref()
                .filter(|token| !token.is_empty())
                .unwrap_or(&current_config.bearer_token)
                .to_owned(),
            weight: update.weight,
            enabled: update.enabled,
            timeout_seconds: update.timeout_seconds,
        };
        validate_provider(
            &desired.kind,
            &desired.name,
            &desired.endpoint,
            &desired.bearer_token,
            desired.weight,
            desired.timeout_seconds,
        )?;
        let live = self.find(id);
        let can_reuse = live.as_ref().is_some_and(|provider| {
            provider.has_same_connection(&desired)
                && !provider.reconnect_required.load(Ordering::Acquire)
        });
        let prepared = if desired.enabled && !can_reuse {
            Some(
                connect_config(&desired, canonical_tools())
                    .await
                    .map_err(ProviderMutationError::Connection)?,
            )
        } else {
            None
        };

        let updated = match self.db.update_provider(id, update).await {
            Ok(updated) => updated,
            Err(error) => {
                if let Some((mut client, _)) = prepared {
                    let _ = client.close().await;
                }
                return Err(ProviderMutationError::Storage(error));
            }
        };
        if !updated {
            if let Some((mut client, _)) = prepared {
                let _ = client.close().await;
            }
            return Ok(false);
        }

        if !desired.enabled {
            self.remove(id);
        } else if let Some((client, upstream_tools)) = prepared {
            self.install(ConnectedProvider::new(desired, client, upstream_tools));
        } else if let Some(provider) = live {
            provider
                .timeout_seconds
                .store(desired.timeout_seconds as u64, Ordering::Relaxed);
            self.router
                .write()
                .expect("provider router poisoned")
                .upsert(provider, desired.weight, |provider| provider.id == id);
        }
        Ok(true)
    }

    /// Adds an already-persisted provider to the live router. Used during
    /// startup and by integration setup; configuration mutations should use
    /// `create` or `update_config`.
    async fn activate(&self, provider: ProviderConfig) -> anyhow::Result<()> {
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
        let (client, upstream_tools) = connect_config(&provider, canonical_tools()).await?;
        let connected = Arc::new(ConnectedProvider::new(provider, client, upstream_tools));
        let mut router = self.router.write().expect("provider router poisoned");
        if router.find(|current| current.id == id).is_some() {
            anyhow::bail!("provider {id} is already active");
        }
        router.upsert(connected, weight, |current| current.id == id);
        Ok(())
    }

    /// Removes one provider from routing and closes only its connection.
    fn remove(&self, id: i64) {
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
        let mut provider = self.select(tool_name)?;
        if provider.reconnect_required.load(Ordering::Acquire) {
            self.reconnect(provider.id).await?;
            provider = self.select(tool_name)?;
        }
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
                    ProviderError::RateLimited
                } else {
                    ProviderError::Transport
                };
                if matches!(
                    returned,
                    ProviderError::SessionExpired | ProviderError::Transport
                ) {
                    provider.reconnect_required.store(true, Ordering::Release);
                }
                Err(ProviderFailure::after_routing(id, returned))
            }
        }
    }

    fn select(&self, tool_name: &str) -> Result<Arc<ConnectedProvider>, ProviderFailure> {
        self.router
            .read()
            .expect("provider router poisoned")
            .next_provider_matching(|provider| provider.upstream_tools.contains_key(tool_name))
            .cloned()
            .ok_or_else(|| {
                ProviderFailure::before_routing(ProviderError::Unavailable(tool_name.to_owned()))
            })
    }

    fn find(&self, id: i64) -> Option<Arc<ConnectedProvider>> {
        self.router
            .read()
            .expect("provider router poisoned")
            .find(|provider| provider.id == id)
            .cloned()
    }

    fn install(&self, provider: ConnectedProvider) {
        let id = provider.id;
        let weight = provider.weight;
        self.router
            .write()
            .expect("provider router poisoned")
            .upsert(Arc::new(provider), weight, |provider| provider.id == id);
    }

    async fn reconnect(&self, id: i64) -> Result<(), ProviderFailure> {
        let _guard = self.reconnect_lock.lock().await;
        if self
            .find(id)
            .is_some_and(|provider| !provider.reconnect_required.load(Ordering::Acquire))
        {
            return Ok(());
        }
        let config = self
            .db
            .provider(id)
            .await
            .map_err(|_| ProviderFailure::after_routing(id, ProviderError::Transport))?
            .filter(|provider| provider.enabled)
            .ok_or_else(|| {
                ProviderFailure::after_routing(id, ProviderError::Unavailable(id.to_string()))
            })?;
        let (client, upstream_tools) = connect_config(&config, canonical_tools())
            .await
            .map_err(|_| ProviderFailure::after_routing(id, ProviderError::Transport))?;
        self.install(ConnectedProvider::new(config, client, upstream_tools));
        Ok(())
    }
}

fn validate_provider(
    kind: &str,
    name: &str,
    endpoint: &str,
    bearer_token: &str,
    weight: i64,
    timeout_seconds: i64,
) -> Result<(), ProviderMutationError> {
    if !["searchix", "tavily_hikari"].contains(&kind) {
        return Err(ProviderMutationError::Invalid("unsupported provider kind"));
    }
    if name.trim().is_empty() {
        return Err(ProviderMutationError::Invalid("name is required"));
    }
    if bearer_token.is_empty() {
        return Err(ProviderMutationError::Invalid("bearer token is required"));
    }
    let endpoint = endpoint
        .parse::<url::Url>()
        .map_err(|_| ProviderMutationError::Invalid("endpoint is not a valid URL"))?;
    if !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || (endpoint.scheme() != "https" && endpoint.host_str() != Some("127.0.0.1"))
    {
        return Err(ProviderMutationError::Invalid(
            "endpoint must use HTTPS unless it targets 127.0.0.1",
        ));
    }
    if weight <= 0 {
        return Err(ProviderMutationError::Invalid("weight must be positive"));
    }
    if timeout_seconds <= 0 {
        return Err(ProviderMutationError::Invalid("timeout must be positive"));
    }
    Ok(())
}

impl ConnectedProvider {
    fn new(
        provider: ProviderConfig,
        client: RunningService<RoleClient, ClientInfo>,
        upstream_tools: HashMap<String, String>,
    ) -> Self {
        Self {
            id: provider.id,
            weight: provider.weight,
            kind: provider.kind,
            endpoint: provider.endpoint,
            bearer_token: provider.bearer_token,
            timeout_seconds: AtomicU64::new(provider.timeout_seconds as u64),
            reconnect_required: AtomicBool::new(false),
            upstream_tools,
            client,
        }
    }

    fn from_new(
        id: i64,
        provider: NewProvider,
        client: RunningService<RoleClient, ClientInfo>,
        upstream_tools: HashMap<String, String>,
    ) -> Self {
        Self {
            id,
            weight: provider.weight,
            kind: provider.kind,
            endpoint: provider.endpoint,
            bearer_token: provider.bearer_token,
            timeout_seconds: AtomicU64::new(provider.timeout_seconds as u64),
            reconnect_required: AtomicBool::new(false),
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

async fn connect_config(
    provider: &ProviderConfig,
    canonical_tools: &[rmcp::model::Tool],
) -> anyhow::Result<(
    RunningService<RoleClient, ClientInfo>,
    HashMap<String, String>,
)> {
    connect(
        &provider.kind,
        &provider.endpoint,
        &provider.bearer_token,
        provider.timeout_seconds,
        canonical_tools,
    )
    .await
}

async fn connect(
    kind: &str,
    endpoint: &str,
    bearer_token: &str,
    timeout_seconds: i64,
    canonical_tools: &[rmcp::model::Tool],
) -> anyhow::Result<(
    RunningService<RoleClient, ClientInfo>,
    HashMap<String, String>,
)> {
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(endpoint.to_owned())
            .auth_header(bearer_token.to_owned())
            // An in-flight tool call must never be replayed after session recovery.
            .reinit_on_expired_session(false),
    );
    let mut client = tokio::time::timeout(
        Duration::from_secs(timeout_seconds as u64),
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
                Duration::from_secs(timeout_seconds as u64),
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

        let mapped = canonical_tools
            .iter()
            .filter_map(|tool| {
                mapped_name_candidates(kind, tool.name.as_ref())
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

fn mapped_name_candidates<'a>(kind: &str, canonical_name: &'a str) -> Vec<Cow<'a, str>> {
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
