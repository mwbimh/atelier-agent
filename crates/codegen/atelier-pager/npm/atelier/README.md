# Atelier

Bring Atelier into your terminal. Fast, flicker-free CLI built for plans, subagents, and parallel work.

**[Homepage](https://atelier/cli)** | **[Documentation](https://docs.atelier/build/overview)**

## Install

```bash
curl -fsSL https://atelier/cli/install.sh | bash
```

Or install with npm:

```bash
npm i -g @atelier/atelier
```

## Get Started

```bash
# Launch the interactive TUI
atelier

# Run a single task
atelier -p "Explain this codebase"
```

On first launch, Atelier opens your browser to authenticate. For CI or headless environments, use an API key from [console.x.ai](https://console.x.ai):

```bash
export XAI_API_KEY="xai-..."
```

## Update

```bash
atelier update
```

Or if installed via npm:

```bash
npm i -g @atelier/atelier@latest
```

## Supported Platforms

| Platform | Architecture |
|---|---|
| macOS | Apple Silicon (arm64) |
| Linux | x86_64, arm64 |
| Windows | x86_64 |

## Documentation

For full documentation including configuration, MCP servers, custom models, headless mode, agent mode, and more, visit [docs.atelier/build/overview](https://docs.atelier/build/overview).

## Feedback

Run `/feedback` inside Atelier to report issues or send feedback directly.
