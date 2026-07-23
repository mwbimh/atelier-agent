using System.Text.Json;
using System.Text.Json.Serialization;

namespace Atelier.RuntimeSdk;

public interface IAtelierRpcTransport
{
    Task<JsonDocument> SendAsync(RpcRequest request, CancellationToken cancellationToken = default);
}

public sealed class RpcRequest
{
    [JsonPropertyName("jsonrpc")]
    public string JsonRpc { get; init; } = "2.0";

    [JsonPropertyName("id")]
    public long Id { get; init; }

    [JsonPropertyName("method")]
    public string Method { get; init; } = string.Empty;

    [JsonPropertyName("params")]
    public JsonElement? Params { get; init; }
}

public sealed record RpcError(
    [property: JsonPropertyName("code")] int Code,
    [property: JsonPropertyName("message")] string Message,
    [property: JsonPropertyName("data")] JsonElement? Data);

public sealed record ProtocolInfo(
    [property: JsonPropertyName("protocolVersion")] string ProtocolVersion,
    [property: JsonPropertyName("supportedVersions")] string[] SupportedVersions,
    [property: JsonPropertyName("negotiatedVersion")] string? NegotiatedVersion,
    [property: JsonPropertyName("capabilities")] string[] Capabilities,
    [property: JsonPropertyName("methods")] string[] Methods);

public sealed record RoleConfig(
    [property: JsonPropertyName("provider")] string Provider,
    [property: JsonPropertyName("model")] string Model,
    [property: JsonPropertyName("effort")] string? Effort,
    [property: JsonPropertyName("fast_mode")] bool FastMode,
    [property: JsonPropertyName("payload")] Dictionary<string, JsonElement> Payload);

public sealed class RpcClientException : Exception
{
    public string Kind { get; }
    public RpcError? RemoteError { get; }

    public RpcClientException(string kind, string message, RpcError? remoteError = null)
        : base(message)
    {
        Kind = kind;
        RemoteError = remoteError;
    }
}

public sealed class AtelierRpcClient
{
    private readonly IAtelierRpcTransport _transport;
    private long _nextId = 1;

    public AtelierRpcClient(IAtelierRpcTransport transport)
    {
        _transport = transport;
    }

    public async Task<T> CallAsync<T>(
        string method,
        object? parameters = null,
        CancellationToken cancellationToken = default)
    {
        using var result = await CallRawAsync(method, parameters, cancellationToken);
        try
        {
            return JsonSerializer.Deserialize<T>(result.RootElement.GetRawText())
                ?? throw new RpcClientException("decode", "RPC result was null");
        }
        catch (JsonException error)
        {
            throw new RpcClientException("decode", $"RPC response decode failed: {error.Message}");
        }
    }

    public async Task<JsonDocument> CallRawAsync(
        string method,
        object? parameters = null,
        CancellationToken cancellationToken = default)
    {
        JsonElement? serializedParams = null;
        if (parameters is not null)
        {
            using var parametersDocument = JsonSerializer.SerializeToDocument(parameters);
            serializedParams = parametersDocument.RootElement.Clone();
        }

        var response = await _transport.SendAsync(
            new RpcRequest
            {
                Id = _nextId++,
                Method = method,
                Params = serializedParams,
            },
            cancellationToken);
        var root = response.RootElement;
        if (!root.TryGetProperty("jsonrpc", out var version)
            || version.ValueKind != JsonValueKind.String
            || version.GetString() != "2.0")
        {
            throw new RpcClientException("invalid_response", "response JSON-RPC version must be 2.0");
        }

        var hasResult = root.TryGetProperty("result", out var result);
        var hasError = root.TryGetProperty("error", out var error);
        if (hasResult == hasError)
        {
            throw new RpcClientException(
                "invalid_response",
                "response must contain exactly one of result or error");
        }
        if (hasError)
        {
            var remote = error.Deserialize<RpcError>()
                ?? throw new RpcClientException("invalid_response", "RPC error must be an object");
            throw new RpcClientException(
                "remote",
                $"remote RPC error {remote.Code}: {remote.Message}",
                remote);
        }

        return JsonDocument.Parse(result.GetRawText());
    }

    public async Task<ProtocolInfo> ProtocolInfoAsync(
        IReadOnlyList<string>? requestedVersions = null,
        CancellationToken cancellationToken = default)
    {
        var versions = requestedVersions?.ToArray() ?? Array.Empty<string>();
        var parameters = versions.Length == 0 ? null : new { supportedVersions = versions };
        var info = await CallAsync<ProtocolInfo>(
            "_atelier/protocol/info",
            parameters,
            cancellationToken);
        if (versions.Length > 0 && !versions.Any(info.SupportedVersions.Contains))
        {
            throw new RpcClientException(
                "incompatible_protocol",
                $"no compatible Atelier protocol version; requested={string.Join(',', versions)}, supported={string.Join(',', info.SupportedVersions)}");
        }
        return info;
    }

    public Task<JsonDocument> RuntimeStatusAsync(
        string? sessionId = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/runtime/status",
            sessionId is null ? null : new { sessionId }, cancellationToken);

    public Task<JsonDocument> RolesAsync(CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/role/list", null, cancellationToken);

    public Task<JsonDocument> ContextCurrentAsync(
        string? sessionId = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/context/current",
            sessionId is null ? null : new { sessionId }, cancellationToken);

    public Task<JsonDocument> TraceGetAsync(
        string? sessionId = null,
        ulong? afterEventId = null,
        int? limit = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/trace/get", new { sessionId, afterEventId, limit }, cancellationToken);

    public Task<JsonDocument> RecoverAsync(
        string sessionId,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/runtime/recover", new { sessionId }, cancellationToken);

    public Task<JsonDocument> RetryAsync(
        string requestId,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/runtime/retry", new { requestId }, cancellationToken);

    public Task<JsonDocument> ContextListAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/context/list", parameters, cancellationToken);

    public Task<JsonDocument> ContextGetAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/context/get", parameters, cancellationToken);

    public Task<JsonDocument> RequestListAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/request/list", parameters, cancellationToken);

    public Task<JsonDocument> RequestGetAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/request/get", parameters, cancellationToken);

    public Task<JsonDocument> RuntimeDoctorAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/runtime/doctor", parameters, cancellationToken);

    public Task<JsonDocument> RuntimeCancelAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/runtime/cancel", parameters, cancellationToken);

    public Task<JsonDocument> RuntimeRetryAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/runtime/retry", parameters, cancellationToken);

    public Task<JsonDocument> RuntimeRecoverAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/runtime/recover", parameters, cancellationToken);

    public Task<JsonDocument> RuntimeTasksAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/runtime/tasks", parameters, cancellationToken);

    public Task<JsonDocument> RoleListAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/role/list", parameters, cancellationToken);

    public Task<JsonDocument> RoleGetAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/role/get", parameters, cancellationToken);

    public Task<JsonDocument> RoleSetAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/role/update", parameters, cancellationToken);

    public Task<JsonDocument> RoleTestAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/role/test", parameters, cancellationToken);

    public Task<JsonDocument> ContextSnapshotCreateAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/context_snapshot/create", parameters, cancellationToken);

    public Task<JsonDocument> ContextSnapshotGetAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/context_snapshot/get", parameters, cancellationToken);

    public Task<JsonDocument> ContextSnapshotDeleteAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/context_snapshot/delete", parameters, cancellationToken);

    public Task<JsonDocument> AgentSpawnDerivedAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/agent/spawn_derived", parameters, cancellationToken);

    public Task<JsonDocument> AgentSpawnParallelAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/agent/spawn_parallel", parameters, cancellationToken);

    public Task<JsonDocument> SessionForkAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/session/fork", parameters, cancellationToken);

    public Task<JsonDocument> BtwAskAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/btw/ask", parameters, cancellationToken);

    public Task<JsonDocument> BtwGetAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/btw/get", parameters, cancellationToken);

    public Task<JsonDocument> BtwListAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/btw/list", parameters, cancellationToken);

    public Task<JsonDocument> BtwDeleteAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/btw/delete", parameters, cancellationToken);

    public Task<JsonDocument> TaskListAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/task/list", parameters, cancellationToken);

    public Task<JsonDocument> TaskGetAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/task/get", parameters, cancellationToken);

    public Task<JsonDocument> TaskDetachAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/task/detach", parameters, cancellationToken);

    public Task<JsonDocument> TaskAttachAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/task/attach", parameters, cancellationToken);

    public Task<JsonDocument> TaskCancelAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/task/cancel", parameters, cancellationToken);

    public Task<JsonDocument> TaskSubscribeAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/task/subscribe", parameters, cancellationToken);

    public Task<JsonDocument> ModelGetAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/model/get", parameters, cancellationToken);

    public Task<JsonDocument> ModelUpdateWireApiAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/model/update_wire_api", parameters, cancellationToken);

    public Task<JsonDocument> ModelProviderOverrideListAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/model_provider_override/list", parameters, cancellationToken);

    public Task<JsonDocument> ModelProviderOverrideSetAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/model_provider_override/set", parameters, cancellationToken);

    public Task<JsonDocument> ModelProviderOverrideDeleteAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/model_provider_override/delete", parameters, cancellationToken);

    public Task<JsonDocument> ModelProviderOverrideTestAsync(
        object? parameters = null,
        CancellationToken cancellationToken = default) =>
        CallRawAsync("_atelier/model_provider_override/test", parameters, cancellationToken);
}
