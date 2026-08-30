use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
    time::Instant,
};

use anyhow::Context;
use rmcp::service::RunningService;
use rmcp::{
    ClientLifecycleMode, ClientServiceExt, RoleClient,
    model::{
        CallToolRequestParams, CallToolResult, ClientInfo, ContentBlock, PaginatedRequestParams,
        ProtocolVersion, Tool,
    },
    transport::{
        StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Map, Value};

use crate::{
    catalog::canonical_tools,
    db::{Database, NewProvider, ProviderConfig, ProviderKind, ProviderUpdate, RequestRecord},
    mcp::SUPPORTED_PROTOCOL_VERSIONS,
    router::WeightedRandom,
};

#[derive(Debug, thiserror::Error)]
pub enum ToolCallError {
    #[error("unknown tool")]
    UnknownTool,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("no connected provider currently supports {0}")]
    Unavailable(String),
    #[error("provider request failed")]
    Failed(#[source] anyhow::Error),
}

impl ProviderError {
    pub fn category(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "unavailable",
            Self::Failed(_) => "provider_error",
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
    kind: ProviderKind,
    endpoint: String,
    bearer_token: String,
    upstream_tools: HashMap<String, String>,
    client: RunningService<RoleClient, ClientInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProviderToolMapping {
    pub canonical_tool: String,
    pub upstream_tool: String,
}

pub struct ProviderManager {
    db: Database,
    router: RwLock<WeightedRandom<Arc<ConnectedProvider>>>,
}

impl ProviderManager {
    pub async fn new(db: Database) -> anyhow::Result<Self> {
        let registry = Self {
            db: db.clone(),
            router: RwLock::new(WeightedRandom::default()),
        };
        for provider in db.providers().await? {
            let description = format!("{} ({})", provider.name, provider.id);
            registry
                .activate(provider)
                .await
                .with_context(|| format!("failed to connect provider {description}"))?;
        }
        Ok(registry)
    }

    /// Validates an enabled provider before persisting it, then installs the
    /// already-connected client into the live router.
    pub async fn create(&self, provider: NewProvider) -> Result<i64, ProviderMutationError> {
        validate_provider(
            &provider.name,
            &provider.endpoint,
            &provider.bearer_token,
            provider.weight,
        )?;
        let prepared = if provider.enabled {
            Some(
                connect(
                    provider.kind,
                    &provider.endpoint,
                    &provider.bearer_token,
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
        };
        validate_provider(
            &desired.name,
            &desired.endpoint,
            &desired.bearer_token,
            desired.weight,
        )?;
        let live = self.find(id);
        let can_reuse = live
            .as_ref()
            .is_some_and(|provider| provider.has_same_connection(&desired));
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

    pub fn tools(&self) -> &[Tool] {
        canonical_tools()
    }

    /// Returns the tool-name mappings advertised by a live provider. `None`
    /// means the provider is not currently connected; an active provider only
    /// contains mappings it actually advertised during `tools/list`.
    pub fn tool_mappings(&self, id: i64) -> Option<Vec<ProviderToolMapping>> {
        let provider = self.find(id)?;
        Some(
            canonical_tools()
                .iter()
                .filter_map(|tool| {
                    provider
                        .upstream_tools
                        .get(tool.name.as_ref())
                        .map(|upstream_tool| ProviderToolMapping {
                            canonical_tool: tool.name.to_string(),
                            upstream_tool: upstream_tool.clone(),
                        })
                })
                .collect(),
        )
    }

    pub async fn call_tool(
        &self,
        client_key_id: i64,
        tool_name: &str,
        arguments: Map<String, Value>,
    ) -> Result<CallToolResult, ToolCallError> {
        if !canonical_tools().iter().any(|tool| tool.name == tool_name) {
            return Err(ToolCallError::UnknownTool);
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let started = Instant::now();
        if let Err(error) = self.db.mark_client_key_used(client_key_id).await {
            tracing::warn!(%error, client_key_id, "could not update client API key usage");
        }

        let result = match self.call(tool_name, arguments).await {
            Ok((provider_id, result)) => {
                let provider_error = result.is_error.unwrap_or(false);
                self.record(
                    &request_id,
                    client_key_id,
                    Some(provider_id),
                    started.elapsed().as_millis() as i64,
                    if provider_error { "failure" } else { "success" },
                    provider_error.then_some("upstream_tool_error"),
                )
                .await;
                result
            }
            Err(error) => {
                self.record(
                    &request_id,
                    client_key_id,
                    error.provider_id,
                    started.elapsed().as_millis() as i64,
                    "failure",
                    Some(error.category()),
                )
                .await;
                CallToolResult::error(vec![ContentBlock::text(error.to_string())])
            }
        };
        Ok(result)
    }

    pub async fn call(
        &self,
        tool_name: &str,
        arguments: Map<String, Value>,
    ) -> Result<(i64, CallToolResult), ProviderFailure> {
        let provider = self.select(tool_name)?;
        let id = provider.id;
        let upstream_name = provider
            .upstream_tools
            .get(tool_name)
            .expect("selected provider supports requested tool")
            .clone();
        let params = CallToolRequestParams::new(upstream_name).with_arguments(arguments);
        match provider.client.call_tool(params).await {
            Ok(result) => Ok((id, result)),
            Err(error) => {
                tracing::warn!(
                    provider_id = id,
                    error = %error,
                    "provider tool call failed"
                );
                Err(ProviderFailure::after_routing(
                    id,
                    ProviderError::Failed(anyhow::Error::new(error)),
                ))
            }
        }
    }

    async fn record(
        &self,
        request_id: &str,
        client_key_id: i64,
        provider_id: Option<i64>,
        duration_ms: i64,
        outcome: &str,
        error_category: Option<&str>,
    ) {
        tracing::info!(
            request_id,
            client_key_id,
            provider_id,
            duration_ms,
            outcome,
            error_category,
            "canonical tool call completed"
        );
        if let Err(error) = self
            .db
            .record_request(RequestRecord {
                id: request_id,
                client_key_id,
                provider_id,
                duration_ms,
                outcome,
                error_category,
            })
            .await
        {
            tracing::error!(%error, request_id, "could not persist tool call metadata");
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
}

fn validate_provider(
    name: &str,
    endpoint: &str,
    bearer_token: &str,
    weight: i64,
) -> Result<(), ProviderMutationError> {
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
        provider.kind,
        &provider.endpoint,
        &provider.bearer_token,
        canonical_tools,
    )
    .await
}

async fn connect(
    kind: ProviderKind,
    endpoint: &str,
    bearer_token: &str,
    canonical_tools: &[rmcp::model::Tool],
) -> anyhow::Result<(
    RunningService<RoleClient, ClientInfo>,
    HashMap<String, String>,
)> {
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(endpoint.to_owned())
            .auth_header(bearer_token.to_owned()),
    );
    let mut client = ClientInfo::default()
        .serve_with_lifecycle(
            transport,
            ClientLifecycleMode::Auto {
                preferred_versions: SUPPORTED_PROTOCOL_VERSIONS.iter().rev().cloned().collect(),
                legacy_version: Some(ProtocolVersion::V_2025_11_25),
            },
        )
        .await?;
    let catalog = async {
        let mut cursor = None;
        let mut upstream_names = HashSet::new();
        loop {
            let params = cursor
                .clone()
                .map(|value| PaginatedRequestParams::default().with_cursor(Some(value)));
            let page = client.list_tools(params).await?;
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
                    .find(|candidate| upstream_names.contains(candidate.as_str()))
                    .map(|candidate| (tool.name.to_string(), candidate.into_string()))
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

#[derive(Debug, PartialEq, Eq)]
enum MappedNameCandidate {
    Prefixed(String),
    Canonical(String),
}

impl MappedNameCandidate {
    fn as_str(&self) -> &str {
        match self {
            Self::Prefixed(name) | Self::Canonical(name) => name,
        }
    }

    fn into_string(self) -> String {
        match self {
            Self::Prefixed(name) | Self::Canonical(name) => name,
        }
    }
}

fn mapped_name_candidates(kind: ProviderKind, canonical_name: &str) -> Vec<MappedNameCandidate> {
    match kind {
        ProviderKind::Searchix => vec![
            MappedNameCandidate::Prefixed(format!("search_proxy_{canonical_name}")),
            MappedNameCandidate::Canonical(canonical_name.to_owned()),
        ],
        ProviderKind::TavilyHikari => {
            vec![MappedNameCandidate::Canonical(canonical_name.to_owned())]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name_candidates_cover_every_canonical_tool() {
        for tool in canonical_tools() {
            assert_eq!(
                mapped_name_candidates(ProviderKind::Searchix, tool.name.as_ref()),
                vec![
                    MappedNameCandidate::Prefixed(format!("search_proxy_{}", tool.name)),
                    MappedNameCandidate::Canonical(tool.name.to_string()),
                ]
            );
            assert_eq!(
                mapped_name_candidates(ProviderKind::TavilyHikari, tool.name.as_ref()),
                vec![MappedNameCandidate::Canonical(tool.name.to_string())]
            );
        }
    }

    #[test]
    fn provider_error_keeps_sdk_source_but_exposes_one_category() {
        let error = ProviderError::Failed(anyhow::Error::msg("sdk detail"));

        assert_eq!(error.category(), "provider_error");
        assert_eq!(error.to_string(), "provider request failed");
        assert_eq!(
            std::error::Error::source(&error).unwrap().to_string(),
            "sdk detail"
        );
    }
}
