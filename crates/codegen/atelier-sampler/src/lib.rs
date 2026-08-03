//! atelier-sampler - Actor-based sampling layer for xAI atelier.
//!
//! This crate extracts the HTTP streaming + retry logic out of
//! `atelier-shell`'s session actor into a standalone, reusable
//! component built on the same actor pattern as `atelier-hunk-tracker`.
//!
//! ## Layered API
//!
//! - **Layer 1**: [`client::SamplingClient`] returns raw chunk streams.
//! - **Layer 2**: [`stream`] transforms raw streams into [`SamplingEvent`]s.
//! - **Layer 3**: [`SamplerHandle`] manages concurrent requests with retry,
//!   cancellation, and event-based coordination via the actor.
//!
//! The type skeleton, the pure retry / metrics / client logic, the
//! Layer-2 stream transforms ([`stream_chat_completions`],
//! [`stream_responses`], [`stream_messages`], [`collect_response`]),
//! and the actor with its per-request task tie these layers together.

pub mod actor;
pub mod attribution;
pub mod client;
pub mod commands;
pub mod compaction;
pub mod config;
pub mod doom_loop;
pub mod events;
pub mod handle;
pub mod image_generation;
pub mod metrics;
pub mod retry;
pub mod sampling_log;
mod shared_http;
pub mod stream;
pub mod types;

// Public re-exports — the API surface consumers see.
pub use actor::SamplerActor;
pub use atelier_sampling_types::SamplingError;
pub use attribution::{
    Auth401AttributionCallback, SENT_BEARER_PREFIX_LEN, SamplingConsumer, SharedAttributionCallback,
};
pub use client::{
    ApiBackend, SamplingClient, request_agent_user_agent_string, set_request_agent_identity,
    set_request_agent_identity_with_user_agent, user_agent_string_for,
};
pub use compaction::{
    CompactFailureAction, RemoteCompactionV2Client, RemoteCompactionV2Output,
    classify_compact_failure,
};
pub use config::{
    AuthScheme, BearerResolver, HeaderInjector, OriginClientInfo, RetryPolicy, SamplerConfig,
    SharedBearerResolver, SharedHeaderInjector,
};
pub use doom_loop::DoomLoopSignalCollector;
pub use events::{SamplingChannel, SamplingErrorInfo, SamplingErrorKind, SamplingEvent};
pub use handle::SamplerHandle;
pub use image_generation::{
    GeneratedImage, ImageEditClient, ImageEditReference, ImageEditRequest, ImageGenerationClient,
    ImageGenerationRequest,
};
pub use metrics::{InferenceLatencyStats, compute_percentiles};
pub use retry::{
    DEFAULT_MAX_RETRIES, MAX_CONFIGURED_RETRIES, RATE_LIMIT_RETRY_THRESHOLD, RetryDecision,
    classify_error, format_sampling_error, resolve_max_retries, retry_backoff_with_jitter,
};
pub use sampling_log::AuthInfo;
pub use stream::{collect_response, stream_chat_completions, stream_messages, stream_responses};
pub use types::RequestId;
