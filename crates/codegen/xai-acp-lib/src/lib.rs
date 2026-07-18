mod channel;
mod common;
mod gateway;
mod line_reader;
mod message;
mod normalize;
mod runtime_control;
mod sdk;
mod stdin_reader;

pub use self::{
    channel::{AcpAgentChannel, AcpChannel, AcpClientChannel, acp_channels, acp_send},
    common::{
        AcpAgentRx, AcpAgentTx, AcpChannelFailure, AcpClientRx, AcpClientTx, AcpResult, AcpRxo,
        AcpTxo, acp_channel_failure, acp_internal_error,
    },
    gateway::{
        AcpAgentGatewayReceiver, AcpAgentGatewaySender, AcpClientGatewayReceiver,
        AcpClientGatewaySender, AcpGatewayReceiver, AcpGatewaySender, acp_gateway,
    },
    message::{
        AcpAgentMessage, AcpAgentMessageBox, AcpAgentMessageGeneric, AcpArgs, AcpArgsBox,
        AcpClientMessage, AcpClientMessageBox, AcpClientMessageGeneric, AcpMethod, AcpRequest,
        AcpSide, Boxed, StorageMarker, Unboxed,
    },
};

pub use self::line_reader::LineBufferedRead;
pub use self::runtime_control::{
    ATELIER_PROTOCOL_CAPABILITIES, ATELIER_PROTOCOL_VERSION, ATELIER_SUPPORTED_PROTOCOL_VERSIONS,
    ContextBlockSummary, ContextSnapshot, DEFAULT_EVENT_REPLAY_CAPACITY, EventId,
    EventReplayBuffer, EventReplayError, EventSequencer, ProtocolInfo, REDACTED_VALUE, ReplayError,
    RequestTraceSummary, RpcError, RpcId, RpcRequest, RpcResponse, RuntimeDiagnostic, RuntimeState,
    SequencedEvent, VersionedProtocol, WatchdogSnapshot, redact_payload, redact_text,
};
pub use self::sdk::{AtelierRpcClient, RpcClientError, RpcTransport};
pub use self::stdin_reader::spawn_stdin_line_reader;

#[doc(hidden)]
pub use self::common::compact_json;
