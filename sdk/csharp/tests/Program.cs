using System.Text.Json;
using Atelier.RuntimeSdk;

var fixturePath = Path.Combine(AppContext.BaseDirectory, "Fixtures", "rpc-contract.json");
using var fixture = JsonDocument.Parse(File.ReadAllText(fixturePath));
var configJson = fixture.RootElement
    .GetProperty("roleListResult")
    .GetProperty("roles")[0]
    .GetProperty("config")
    .GetRawText();

var config = JsonSerializer.Deserialize<RoleConfig>(configJson)
    ?? throw new InvalidOperationException("RoleConfig fixture decoded to null");

if (!config.FastMode)
{
    throw new InvalidOperationException("RoleConfig.fast_mode did not deserialize as FastMode=true");
}

using var serialized = JsonSerializer.SerializeToDocument(config);
if (!serialized.RootElement.TryGetProperty("fast_mode", out var fastMode)
    || fastMode.ValueKind != JsonValueKind.True)
{
    throw new InvalidOperationException("RoleConfig must serialize FastMode as fast_mode");
}
if (serialized.RootElement.TryGetProperty("fastMode", out _))
{
    throw new InvalidOperationException("RoleConfig must not serialize the non-Rust fastMode key");
}

var transport = new RecordingTransport();
var client = new AtelierRpcClient(transport);

await InvokeAsync(client.ContextCurrentAsync());
await InvokeAsync(client.ContextListAsync());
await InvokeAsync(client.ContextGetAsync());
await InvokeAsync(client.RequestListAsync());
await InvokeAsync(client.RequestGetAsync());
await InvokeAsync(client.TraceGetAsync());
await InvokeAsync(client.RuntimeStatusAsync());
await InvokeAsync(client.RuntimeDoctorAsync());
await InvokeAsync(client.RuntimeCancelAsync());
await InvokeAsync(client.RuntimeRetryAsync());
await InvokeAsync(client.RuntimeRecoverAsync());
await InvokeAsync(client.RuntimeTasksAsync());
await InvokeAsync(client.RoleListAsync());
await InvokeAsync(client.RoleGetAsync());
await InvokeAsync(client.RoleSetAsync());
await InvokeAsync(client.RoleTestAsync());
await InvokeAsync(client.ContextSnapshotCreateAsync());
await InvokeAsync(client.ContextSnapshotGetAsync());
await InvokeAsync(client.ContextSnapshotDeleteAsync());
await InvokeAsync(client.AgentSpawnDerivedAsync());
await InvokeAsync(client.AgentSpawnParallelAsync());
await InvokeAsync(client.SessionForkAsync());
await InvokeAsync(client.BtwAskAsync());
await InvokeAsync(client.BtwGetAsync());
await InvokeAsync(client.BtwListAsync());
await InvokeAsync(client.BtwDeleteAsync());
await InvokeAsync(client.TaskListAsync());
await InvokeAsync(client.TaskGetAsync());
await InvokeAsync(client.TaskDetachAsync());
await InvokeAsync(client.TaskAttachAsync());
await InvokeAsync(client.TaskCancelAsync());
await InvokeAsync(client.TaskSubscribeAsync());
await InvokeAsync(client.ModelGetAsync());
await InvokeAsync(client.ModelUpdateWireApiAsync());
await InvokeAsync(client.ModelProviderOverrideListAsync());
await InvokeAsync(client.ModelProviderOverrideSetAsync());
await InvokeAsync(client.ModelProviderOverrideDeleteAsync());
await InvokeAsync(client.ModelProviderOverrideTestAsync());

var expectedMethods = fixture.RootElement
    .GetProperty("convenienceMethods")
    .EnumerateArray()
    .Select(entry => entry.GetProperty("wire").GetString())
    .ToArray();
var actualMethods = transport.Requests.Select(request => request.Method).ToArray();
if (!expectedMethods.SequenceEqual(actualMethods))
{
    throw new InvalidOperationException(
        $"Convenience API wire methods differ. Expected: {string.Join(", ", expectedMethods)}; actual: {string.Join(", ", actualMethods)}");
}

static async Task InvokeAsync(Task<JsonDocument> call)
{
    using var result = await call;
}

sealed class RecordingTransport : IAtelierRpcTransport
{
    public List<RpcRequest> Requests { get; } = [];

    public Task<JsonDocument> SendAsync(
        RpcRequest request,
        CancellationToken cancellationToken = default)
    {
        Requests.Add(request);
        return Task.FromResult(JsonSerializer.SerializeToDocument(new
        {
            jsonrpc = "2.0",
            id = request.Id,
            result = new { },
        }));
    }
}
