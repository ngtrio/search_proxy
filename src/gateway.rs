use std::{sync::Arc, time::Instant};

use rmcp::model::{CallToolResult, ContentBlock, Tool};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::{Database, catalog::canonical_tools, db::RequestRecord, provider::ProviderManager};

#[derive(Debug, thiserror::Error)]
pub enum ToolGatewayError {
    #[error("unknown tool")]
    UnknownTool,
}

pub struct ToolGateway {
    db: Database,
    providers: Arc<ProviderManager>,
}

impl ToolGateway {
    pub fn new(db: Database, providers: Arc<ProviderManager>) -> Self {
        Self { db, providers }
    }

    pub fn tools(&self) -> &[Tool] {
        canonical_tools()
    }

    pub async fn call(
        &self,
        client_key_id: i64,
        tool_name: &str,
        arguments: Map<String, Value>,
    ) -> Result<CallToolResult, ToolGatewayError> {
        if !canonical_tools().iter().any(|tool| tool.name == tool_name) {
            return Err(ToolGatewayError::UnknownTool);
        }

        let request_id = Uuid::new_v4().to_string();
        let started = Instant::now();
        if let Err(error) = self.db.mark_client_key_used(client_key_id).await {
            tracing::warn!(%error, client_key_id, "could not update client API key usage");
        }

        let result = match self.providers.call(tool_name, arguments).await {
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
}
