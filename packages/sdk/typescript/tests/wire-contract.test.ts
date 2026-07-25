import contract from "../../fixtures/rpc-contract.json" with { type: "json" };
import assert from "node:assert/strict";

import {
  AtelierRpcClient,
  type RoleConfig,
  type RpcRequest,
  type RuntimeStatus,
} from "../src/index.js";

type Equal<Left, Right> =
  (<Value>() => Value extends Left ? 1 : 2) extends
  (<Value>() => Value extends Right ? 1 : 2)
    ? true
    : false;
type Assert<Value extends true> = Value;

const roleConfig: RoleConfig = contract.roleListResult.roles[0]!.config;
const runtimeStatus: RuntimeStatus = contract.runtimeStatusResult.statuses[0]!;

type _RoleConfigKeysMatchRustWire = Assert<
  Equal<keyof RoleConfig, keyof typeof contract.roleListResult.roles[0]["config"]>
>;
type _RuntimeStatusKeysMatchRustWire = Assert<
  Equal<keyof RuntimeStatus, keyof typeof contract.runtimeStatusResult.statuses[0]>
>;

void roleConfig;
void runtimeStatus;

const requests: RpcRequest[] = [];
const client = new AtelierRpcClient({
  send: async (request) => {
    requests.push(request);
    return {
      jsonrpc: "2.0",
      id: request.id ?? null,
      result: {},
    };
  },
});

await client.contextCurrent();
await client.contextList();
await client.contextGet();
await client.requestList();
await client.requestGet();
await client.traceGet();
await client.runtimeStatus();
await client.runtimeDoctor();
await client.runtimeCancel();
await client.runtimeRetry();
await client.runtimeRecover();
await client.runtimeTasks();
await client.roleList();
await client.roleGet();
await client.roleSet();
await client.roleTest();
await client.contextSnapshotCreate();
await client.contextSnapshotGet();
await client.contextSnapshotDelete();
await client.agentSpawnDerived();
await client.agentSpawnParallel();
await client.sessionFork();
await client.btwAsk();
await client.btwGet();
await client.btwList();
await client.btwDelete();
await client.taskList();
await client.taskGet();
await client.taskDetach();
await client.taskAttach();
await client.taskCancel();
await client.taskSubscribe();
await client.modelGet();
await client.modelUpdateWireApi();
await client.modelProviderOverrideList();
await client.modelProviderOverrideSet();
await client.modelProviderOverrideDelete();
await client.modelProviderOverrideTest();

assert.deepEqual(
  requests.map(({ method }) => method),
  contract.convenienceMethods.map(({ wire }) => wire),
);
