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
  effort?: string;
  fastMode?: boolean;
  payload: Record<string, unknown>;
}

export interface RuntimeStatus {
  sessionId: string;
  state: string;
  requestId?: string;
  turnId?: string;
  role: RoleId | string;
  provider?: string;
  model?: string;
  retryCount: number;
  diagnosticMessage?: string;
}

export interface RpcTransport {
  send(request: RpcRequest): Promise<RpcResponse>;
}

export class RpcClientError extends Error {
  readonly kind: "transport" | "invalid_response" | "remote" | "decode" | "incompatible_protocol";
  readonly remote?: RpcError;

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
}
