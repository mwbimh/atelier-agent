# Atelier TypeScript SDK

这是一个 transport-neutral thin client。调用方只需实现：

```ts
const client = new AtelierRpcClient({
  send: async (request) => transport.send(request),
});
```

SDK 负责 JSON-RPC `2.0` envelope、`result/error` 严格校验、协议版本协商，以及 Atelier 的 Runtime、Role、Context 和 Trace 扩展调用。stdio/WebSocket 传输由宿主应用提供。
