//! Streaming Responses remote-compaction-v2 transport.
//!
//! V2 uses the ordinary `/responses` endpoint. The request is a normal
//! Responses inference request whose final input item is the raw
//! `{ "type": "compaction_trigger" }` sentinel. The stream must complete and
//! emit exactly one compaction output item.

use std::time::Duration;

use atelier_sampling_types::{
    ApiBackend, ConversationRequest, Result, SamplingError, TokenUsage, rs,
};
use futures_util::StreamExt;

use crate::client::SamplingClient;
use crate::config::SamplerConfig;
use crate::retry::retry_backoff_with_jitter;

const MAX_REMOTE_COMPACTION_V2_STREAM_RETRIES: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactFailureAction {
    FallbackLocal,
    ReturnError,
}

pub fn classify_compact_failure(error: &SamplingError) -> CompactFailureAction {
    match error {
        SamplingError::Http(_)
        | SamplingError::Serialization(_)
        | SamplingError::EventStreamError(_)
        | SamplingError::StreamError { .. } => CompactFailureAction::FallbackLocal,
        SamplingError::Api { status, .. }
            if matches!(status.as_u16(), 404 | 405 | 501) || status.is_server_error() =>
        {
            CompactFailureAction::FallbackLocal
        }
        SamplingError::Auth(_)
        | SamplingError::InvalidConfiguration(_)
        | SamplingError::Api { .. }
        | SamplingError::IdleTimeout { .. }
        | SamplingError::EmptyResponse { .. }
        | SamplingError::MaxTokensTruncation
        | SamplingError::DoomLoopDetected { .. } => CompactFailureAction::ReturnError,
    }
}

#[derive(Clone, Debug)]
pub struct RemoteCompactionV2Output {
    pub compaction: rs::CompactionSummaryItemParam,
    pub response_id: String,
    pub usage: Option<TokenUsage>,
}

/// Exact Provider/model-gated client for Responses remote compaction v2.
#[derive(Clone, Debug)]
pub struct RemoteCompactionV2Client {
    sampling: SamplingClient,
    model: String,
}

impl RemoteCompactionV2Client {
    pub fn from_config(config: SamplerConfig) -> Result<Option<Self>> {
        if !config.remote_compaction_v2 {
            return Ok(None);
        }
        if config.api_backend != ApiBackend::Responses {
            return Err(SamplingError::InvalidConfiguration(
                "remote compaction v2 requires the Responses wire API",
            ));
        }
        let model = config.model.clone();
        let sampling = SamplingClient::new(config)?;
        Ok(Some(Self { sampling, model }))
    }

    pub async fn compact(
        &self,
        request: ConversationRequest,
        instructions: Option<String>,
        request_timeout: Duration,
    ) -> Result<RemoteCompactionV2Output> {
        match tokio::time::timeout(
            request_timeout,
            self.compact_with_retries(request, instructions),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(SamplingError::IdleTimeout {
                elapsed_secs: request_timeout.as_secs(),
            }),
        }
    }

    async fn compact_with_retries(
        &self,
        request: ConversationRequest,
        instructions: Option<String>,
    ) -> Result<RemoteCompactionV2Output> {
        if request
            .model
            .as_deref()
            .is_some_and(|model| model != self.model)
        {
            return Err(SamplingError::InvalidConfiguration(
                "remote compaction v2 request model must match the Provider model",
            ));
        }

        let mut retries = 0u32;
        loop {
            match self
                .compact_once(request.clone(), instructions.clone())
                .await
            {
                Ok(output) => return Ok(output),
                Err(error)
                    if error.is_retryable()
                        && !error.is_context_length_error()
                        && error.should_retry_header() != Some(false)
                        && retries < MAX_REMOTE_COMPACTION_V2_STREAM_RETRIES =>
                {
                    retries += 1;
                    let backoff = error
                        .retry_after()
                        .map(Duration::from_secs)
                        .unwrap_or_else(|| retry_backoff_with_jitter(retries));
                    tokio::time::sleep(backoff).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn compact_once(
        &self,
        mut request: ConversationRequest,
        instructions: Option<String>,
    ) -> Result<RemoteCompactionV2Output> {
        request.model = Some(self.model.clone());
        self.sampling.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let extra_tools = atelier_sampling_types::extra_raw_tools(&request.hosted_tools);
        let responses_request: rs::CreateResponse = (&request).into();
        let mut wrapper = atelier_sampling_types::CreateResponseWrapper::new(responses_request);
        wrapper.reasoning_effort = request.reasoning_effort;
        wrapper.x_atelier_conv_id = request.x_atelier_conv_id;
        wrapper.x_atelier_req_id = request.x_atelier_req_id;
        wrapper.x_atelier_session_id = request.x_atelier_session_id;
        wrapper.x_atelier_turn_idx = request.x_atelier_turn_idx;
        wrapper.x_atelier_agent_id = request.x_atelier_agent_id;
        wrapper.extra_raw_tools = extra_tools;
        wrapper.extra_raw_input_items = vec![serde_json::json!({
            "type": "compaction_trigger"
        })];
        wrapper.inner.instructions = instructions.filter(|value| !value.is_empty());
        if let Some(trace) = trace {
            wrapper.trace = Some(trace);
        }

        let (mut stream, _metadata, _doom_loop) =
            self.sampling.create_response_stream(wrapper).await?;
        collect_compaction_output(&mut stream).await
    }
}

async fn collect_compaction_output(
    stream: &mut (impl futures_util::Stream<Item = Result<rs::ResponseStreamEvent>> + Unpin),
) -> Result<RemoteCompactionV2Output> {
    let mut output_item_count = 0usize;
    let mut compaction_count = 0usize;
    let mut compaction = None;

    while let Some(event) = stream.next().await {
        match event? {
            rs::ResponseStreamEvent::ResponseOutputItemDone(event) => {
                output_item_count += 1;
                if let rs::OutputItem::Compaction(item) = event.item {
                    compaction_count += 1;
                    if compaction.is_none() {
                        compaction = Some(rs::CompactionSummaryItemParam {
                            id: Some(item.id),
                            encrypted_content: item.encrypted_content,
                        });
                    }
                }
            }
            rs::ResponseStreamEvent::ResponseCompleted(event) => {
                if compaction_count != 1 {
                    return Err(SamplingError::serialization_message(format!(
                        "remote compaction v2 expected exactly one compaction output item, got {compaction_count} from {output_item_count} output items"
                    )));
                }
                let usage = event.response.usage.map(|usage| TokenUsage {
                    prompt_tokens: usage.input_tokens,
                    completion_tokens: usage.output_tokens,
                    total_tokens: usage.total_tokens,
                    reasoning_tokens: usage.output_tokens_details.reasoning_tokens,
                    cached_prompt_tokens: usage.input_tokens_details.cached_tokens,
                });
                return Ok(RemoteCompactionV2Output {
                    compaction: compaction.expect("count is exactly one"),
                    response_id: event.response.id,
                    usage,
                });
            }
            rs::ResponseStreamEvent::ResponseIncomplete(_) => {
                return Err(SamplingError::MaxTokensTruncation);
            }
            rs::ResponseStreamEvent::ResponseFailed(event) => {
                let message = event
                    .response
                    .error
                    .map(|error| format!("{}: {}", error.code, error.message))
                    .unwrap_or_else(|| "remote compaction v2 response failed".to_owned());
                return Err(SamplingError::Api {
                    status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                    message,
                    model_metadata: None,
                    retry_after_secs: None,
                    should_retry: None,
                });
            }
            rs::ResponseStreamEvent::ResponseError(event) => {
                return Err(SamplingError::Api {
                    status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                    message: format!(
                        "{}: {}",
                        event.code.unwrap_or_else(|| "error".to_owned()),
                        event.message
                    ),
                    model_metadata: None,
                    retry_after_secs: None,
                    should_retry: None,
                });
            }
            _ => {}
        }
    }

    Err(SamplingError::EventStreamError(
        "remote compaction v2 stream closed before response.completed".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{CompactFailureAction, classify_compact_failure};
    use atelier_sampling_types::SamplingError;
    use reqwest::StatusCode;

    fn api_error(status: StatusCode) -> SamplingError {
        SamplingError::Api {
            status,
            message: "test".to_owned(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
        }
    }

    #[test]
    fn remote_compaction_failure_contract_is_exact() {
        for status in [
            StatusCode::NOT_FOUND,
            StatusCode::METHOD_NOT_ALLOWED,
            StatusCode::NOT_IMPLEMENTED,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
        ] {
            assert_eq!(
                classify_compact_failure(&api_error(status)),
                CompactFailureAction::FallbackLocal
            );
        }
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            assert_eq!(
                classify_compact_failure(&api_error(status)),
                CompactFailureAction::ReturnError
            );
        }
        assert_eq!(
            classify_compact_failure(&SamplingError::serialization_message("decode")),
            CompactFailureAction::FallbackLocal
        );
        assert_eq!(
            classify_compact_failure(&SamplingError::InvalidConfiguration("disabled")),
            CompactFailureAction::ReturnError
        );
    }
}
