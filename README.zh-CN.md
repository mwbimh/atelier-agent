# Atelier

[English documentation](README.md)

Atelier 是一个本地优先的终端编程智能体。本仓库包含 `ate` CLI、可复用的
运行时 crates、多语言 SDK、发布工具，以及为未来桌面 GUI 预留的工作区。

Atelier 基于 Grok Build 代码库继续开发，是一个独立的衍生项目，与 xAI
不存在隶属或官方背书关系。上游归属和修改记录保存在
[第三方声明](THIRD_PARTY_NOTICES.md)及 [`docs/upstream/`](docs/upstream/) 中。

## 当前状态

Atelier 目前处于 Alpha 阶段。Windows x64 单二进制发布和沙箱流程是当前
完成完整验证的发行目标。代码已经包含 Linux 和 macOS 支持，但在正式发布前，
仍需补齐对应平台的原生构建流水线和真实系统端到端验证。

Atelier 尚未向 npm 发布 CLI 或 SDK 包。仓库中的 npm manifests 只是私有的
开发打包脚手架，不是当前可用的发行渠道。

## 核心原则

- **本地优先：** Session、日志、Trace、Metrics 和 Artifact 保存在本机。
- **显式模型配置：** 首次启动不会自动选择 Provider 或模型。
- **与模型厂商解耦：** 模型访问完全由用户配置的 Provider 决定，不存在内置
  厂商账号或模型回退。
- **无远程遥测：** 不包含遥测上传、远程设置、自动更新或 Session 分享服务。
- **单 CLI 二进制：** Workspace Worker 和命令 Runner 通过 `ate` 的内部模式
  运行，不单独发布可执行文件。

## 快速开始

项目使用 [`rust-toolchain.toml`](rust-toolchain.toml) 中固定的 Rust 工具链。

```sh
cargo run -p atelier-pager-bin --bin ate
```

首次启动后，需要先配置 Provider 并选择模型：

```text
/provider
/model
```

Atelier 不会静默选择第一个可用的 Provider 或模型。

常用开发命令：

```sh
cargo check --locked -p atelier-pager-bin
cargo test --locked -p atelier-pager-bin
cargo fmt --all -- --check
```

## Windows 发布

发布产物固定输出到仓库顶层的 `release/`。该目录由 Git 忽略，二进制应通过
GitHub Releases 发布，不应提交到源码历史中。

```powershell
.\tools\build-release.ps1 -CleanOutput
```

Windows 发布包严格包含：

```text
release/
├── ate.exe
└── install-windows.ps1
```

为当前用户安装：

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\release\install-windows.ps1
```

安装脚本支持 `-InstallDir`、`-NoPathUpdate`、`-SetupSandbox` 和
`-SkipDefaultTools`。它会优先复用宿主 PATH 中已有的 Git、ripgrep 和 uv；缺失时才安装固定版本并校验 SHA-256 的 managed copy。Node.js 与 Rust 只做推荐，不会自动安装。

## Monorepo 目录

| 路径 | 用途 |
| --- | --- |
| [`apps/cli/`](apps/cli/) | `ate` 组合入口、集成测试、安装脚本和尚未发布的 npm 打包脚手架 |
| [`apps/gui/`](apps/gui/) | 为未来桌面 GUI 预留的应用工作区 |
| [`packages/sdk/`](packages/sdk/) | TypeScript、C# SDK 和共享协议 fixtures |
| [`crates/`](crates/) | 可复用的 Rust 运行时、TUI、Provider、沙箱、工具和协议 crates |
| [`docs/`](docs/) | 仓库架构和上游源码记录 |
| [`third_party/`](third_party/) | 第三方源码和声明 |
| [`tools/`](tools/) | 构建与发布自动化 |
| `release/` | 本地顶层发布输出，不提交到 Git |

目录归属和新增模块规则见[仓库目录说明](docs/REPOSITORY_LAYOUT.md)。

## SDK

- [TypeScript SDK](packages/sdk/typescript/README.md)
- [C# SDK](packages/sdk/csharp/README.md)

SDK 与 Rust 协议契约测试共享
[`packages/sdk/fixtures/`](packages/sdk/fixtures/) 下的 fixtures。

## 文档

- [CLI 用户指南](crates/codegen/atelier-pager/docs/user-guide/README.md)
- [CLI 应用说明](apps/cli/README.md)
- [运行时架构](crates/codegen/atelier-shell/README.md)
- [Windows 沙箱](crates/codegen/atelier-windows-sandbox/README.md)
- [贡献指南](CONTRIBUTING.md)
- [安全策略](SECURITY.md)

## 参与贡献

项目接受外部贡献。Bug、文档、测试、Provider 集成、SDK 修改以及范围明确的
运行时改动，都可以通过 GitHub Issue 和 Pull Request 提交。开始较大改动前，
请先阅读 [`CONTRIBUTING.md`](CONTRIBUTING.md)。

## 许可证

Atelier 的第一方修改使用 Apache License 2.0。项目继续保留 Grok Build 的
原始许可证和声明，以及其他改编或内置第三方代码的声明。详见
[`LICENSE`](LICENSE)、[`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES) 和
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)。
