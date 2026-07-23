export type RpcId = string | number;

export interface RpcRequest {
  jsonrpc: "2.0";
  id?: RpcId;
  method: string;
  params?: unknown;
}

export interface RpcError {
  code: number;
  message: string;
  data?: unknown;
}

export interface RpcResponse<T = unknown> {
  jsonrpc: "2.0";
  id: RpcId | null;
  result?: T;
  error?: RpcError;
}

export interface ProtocolInfo {
  protocolVersion: string;
  supportedVersions: string[];
  negotiatedVersion?: string;
  capabilities: string[];
  methods: string[];
}

export type RoleId =
  | "main"
  | "explore"
  | "implement"
  | "review"
  | "test"
  | "compact"
  | "summary"
  | "title";

export interface RoleConfig {
  provider: string;
  model: string;
  effort: string | null;
  fast_mode: boolean;
  payload: Record<string, unknown>;
}

export interface RuntimeStatus {
  sessionId: string;
  state: string;
  startedAtMs: number;
  lastProgressAtMs: number;
  requestId: string | null;
  turnId: string | null;
  role: RoleId | string;
  provider: string | null;
  model: string | null;
  timeoutMs: number | null;
  retryCount: number;
  cancelSupported: boolean;
  diagnosticMessage: string | null;
}

export interface RpcTransport {
  send(request: RpcRequest): Promise<RpcResponse>;
}

export class RpcClientError extends Error {
  readonly kind: "transport" | "invalid_response" | "remote" | "decode" | "incompatible_protocol";
  readonly remote: RpcError | undefined;

  constructor(
    kind: RpcClientError["kind"],
    message: string,
    remote?: RpcError,
  ) {
    super(message);
    this.name = "RpcClientError";
    this.kind = kind;
    this.remote = remote;
  }
}

export class AtelierRpcClient {
  private nextId = 1;

  constructor(private readonly transport: RpcTransport) {}

  async call<T>(method: string, params?: unknown): Promise<T> {
    const request: RpcRequest = {
      jsonrpc: "2.0",
      id: this.nextId++,
      method,
      ...(params === undefined ? {} : { params }),
    };

    let response: RpcResponse;
    try {
      response = await this.transport.send(request);
    } catch (error) {
      throw new RpcClientError("transport", String(error));
    }

    if (response?.jsonrpc !== "2.0") {
      throw new RpcClientError("invalid_response", "response JSON-RPC version must be 2.0");
    }
    const hasResult = Object.prototype.hasOwnProperty.call(response, "result");
    const hasError = Object.prototype.hasOwnProperty.call(response, "error");
    if (hasResult === hasError) {
      throw new RpcClientError(
        "invalid_response",
        "response must contain exactly one of result or error",
      );
    }
    if (hasError) {
      const remote = response.error as RpcError;
      throw new RpcClientError("remote", `remote RPC error ${remote.code}: ${remote.message}`, remote);
    }
    return response.result as T;
  }

  async protocolInfo(requestedVersions: string[] = []): Promise<ProtocolInfo> {
    const params = requestedVersions.length === 0 ? undefined : { supportedVersions: requestedVersions };
    const info = await this.call<ProtocolInfo>("_atelier/protocol/info", params);
    if (
      requestedVersions.length > 0 &&
      !requestedVersions.some((version) => info.supportedVersions.includes(version))
    ) {
      throw new RpcClientError(
        "incompatible_protocol",
        `no compatible Atelier protocol version; requested=${requestedVersions.join(",")}, supported=${info.supportedVersions.join(",")}`,
      );
    }
    return info;
  }

  runtimeStatus(sessionId?: string): Promise<unknown> {
    return this.call("_atelier/runtime/status", sessionId === undefined ? undefined : { sessionId });
  }

  roles(): Promise<{ roles: Array<{ roleId: RoleId; config: RoleConfig }> }> {
    return this.call("_atelier/role/list");
  }

  contextCurrent(sessionId?: string): Promise<unknown> {
    return this.call("_atelier/context/current", sessionId === undefined ? undefined : { sessionId });
  }

  requestList(sessionId?: string): Promise<unknown> {
    return this.call("_atelier/request/list", sessionId === undefined ? undefined : { sessionId });
  }

  traceGet(options: {
    sessionId?: string;
    afterEventId?: number;
    limit?: number;
  } = {}): Promise<unknown> {
    return this.call("_atelier/trace/get", options);
  }

  recover(sessionId: string): Promise<unknown> {
    return this.call("_atelier/runtime/recover", { sessionId });
  }

  retry(requestId: string): Promise<unknown> {
    return this.call("_atelier/runtime/retry", { requestId });
  }

  contextList(params?: unknown): Promise<unknown> {
    return this.call("_atelier/context/list", params);
  }

  contextGet(params?: unknown): Promise<unknown> {
    return this.call("_atelier/context/get", params);
  }

  requestGet(params?: unknown): Promise<unknown> {
    return this.call("_atelier/request/get", params);
  }

  runtimeDoctor(params?: unknown): Promise<unknown> {
    return this.call("_atelier/runtime/doctor", params);
  }

  runtimeCancel(params?: unknown): Promise<unknown> {
    return this.call("_atelier/runtime/cancel", params);
  }

  runtimeRetry(params?: unknown): Promise<unknown> {
    return this.call("_atelier/runtime/retry", params);
  }

  runtimeRecover(params?: unknown): Promise<unknown> {
    return this.call("_atelier/runtime/recover", params);
  }

  runtimeTasks(params?: unknown): Promise<unknown> {
    return this.call("_atelier/runtime/tasks", params);
  }

  roleList(params?: unknown): Promise<unknown> {
    return this.call("_atelier/role/list", params);
  }

  roleGet(params?: unknown): Promise<unknown> {
    return this.call("_atelier/role/get", params);
  }

  roleSet(params?: unknown): Promise<unknown> {
    return this.call("_atelier/role/update", params);
  }

  roleTest(params?: unknown): Promise<unknown> {
    return this.call("_atelier/role/test", params);
  }

  contextSnapshotCreate(params?: unknown): Promise<unknown> {
    return this.call("_atelier/context_snapshot/create", params);
  }

  contextSnapshotGet(params?: unknown): Promise<unknown> {
    return this.call("_atelier/context_snapshot/get", params);
  }

  contextSnapshotDelete(params?: unknown): Promise<unknown> {
    return this.call("_atelier/context_snapshot/delete", params);
  }

  agentSpawnDerived(params?: unknown): Promise<unknown> {
    return this.call("_atelier/agent/spawn_derived", params);
  }

  agentSpawnParallel(params?: unknown): Promise<unknown> {
    return this.call("_atelier/agent/spawn_parallel", params);
  }

  sessionFork(params?: unknown): Promise<unknown> {
    return this.call("_atelier/session/fork", params);
  }

  btwAsk(params?: unknown): Promise<unknown> {
    return this.call("_atelier/btw/ask", params);
  }

  btwGet(params?: unknown): Promise<unknown> {
    return this.call("_atelier/btw/get", params);
  }

  btwList(params?: unknown): Promise<unknown> {
    return this.call("_atelier/btw/list", params);
  }

  btwDelete(params?: unknown): Promise<unknown> {
    return this.call("_atelier/btw/delete", params);
  }

  taskList(params?: unknown): Promise<unknown> {
    return this.call("_atelier/task/list", params);
  }

  taskGet(params?: unknown): Promise<unknown> {
    return this.call("_atelier/task/get", params);
  }

  taskDetach(params?: unknown): Promise<unknown> {
    return this.call("_atelier/task/detach", params);
  }

  taskAttach(params?: unknown): Promise<unknown> {
    return this.call("_atelier/task/attach", params);
  }

  taskCancel(params?: unknown): Promise<unknown> {
    return this.call("_atelier/task/cancel", params);
  }

  taskSubscribe(params?: unknown): Promise<unknown> {
    return this.call("_atelier/task/subscribe", params);
  }

  modelGet(params?: unknown): Promise<unknown> {
    return this.call("_atelier/model/get", params);
  }

  modelUpdateWireApi(params?: unknown): Promise<unknown> {
    return this.call("_atelier/model/update_wire_api", params);
  }

  modelProviderOverrideList(params?: unknown): Promise<unknown> {
    return this.call("_atelier/model_provider_override/list", params);
  }

  modelProviderOverrideSet(params?: unknown): Promise<unknown> {
    return this.call("_atelier/model_provider_override/set", params);
  }

  modelProviderOverrideDelete(params?: unknown): Promise<unknown> {
    return this.call("_atelier/model_provider_override/delete", params);
  }

  modelProviderOverrideTest(params?: unknown): Promise<unknown> {
    return this.call("_atelier/model_provider_override/test", params);
  }
}
