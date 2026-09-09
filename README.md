# Optimus

**An open-source coding and research agent, maintained by Telemus AI.**

[Documentation](packages/coding-agent/docs/index.md) · [Telegram setup](packages/coding-agent/docs/telegram.md) · [Development](packages/coding-agent/docs/development.md)

Optimus brings code, tools, and persistent context into one workspace. Use it to explore a repository, implement changes, investigate a problem, or carry a task across multiple sessions. A persistent Python environment lets the agent inspect data, run commands, call skills, and coordinate subagents through code.

## What Optimus Can Do

- **Work with your code and data.** Read and edit files, run project commands, and retain useful variables in a persistent Python REPL.
- **Coordinate subagents.** Delegate independent work to recursive agents that can run in parallel, exchange messages, and report results.
- **Keep sessions running.** Background workers preserve active work when the terminal disconnects, so you can reattach later.
- **Manage long tasks.** Compaction, persistent goals, heartbeats, and schedules help work continue across turns. Autonomous mode supports explicit turn, token, and time limits.
- **Improve reusable instructions.** `/refine` can update supplemental prompts, memories, skill descriptions, and subagent specifications, with recorded history and rollback.
- **Extend the workspace.** Add executable Python skills, prompt templates, TypeScript extensions, and MCP integrations.
- **Connect through Telegram.** Pair a private bot chat with a session and use familiar commands to manage work from your phone.

## Getting Started

Run from source on macOS or Linux with Node.js 22.9.0 or newer and npm 11.10.0 or newer:

```bash
git clone https://github.com/telemusai/prime-agent.git optimus
cd optimus
npm ci
./prime-agent.sh
```

The source launcher is currently named `prime-agent.sh`. These instructions use the existing launcher; the project name is Optimus.

On first launch, use `/login` to configure a provider. Choose a model with `/model`, set its reasoning level with `/effort`, and enter a request:

```text
Explain this repository, identify its main components, and tell me how to run its checks.
```

The Python runtime is prepared automatically on first use. See the [provider guide](packages/coding-agent/docs/providers.md) for authentication options and the [Windows guide](packages/coding-agent/docs/windows.md) for platform-specific setup.

> [!WARNING]
> Optimus can execute model-generated code and project commands with your user permissions. Worker processes are not a security sandbox. Use trusted repositories, instructions, skills, and extensions, and keep changes reviewable with Git or another checkpointing workflow. Run untrusted workloads in an external sandbox or restricted environment.

## Everyday Commands

| Command | Purpose |
| --- | --- |
| `/login` | Configure provider authentication |
| `/model`, `/effort` | Choose a model and reasoning level |
| `/new`, `/resume` | Start a session or return to previous work |
| `/name`, `/session` | Name a session or inspect its status |
| `/context`, `/usage` | Review context, token usage, and cost |
| `/compact` | Compact the session context |
| `/goal` | Set or manage a persistent objective |
| `/autonomous` | Configure autonomous continuation |
| `/heartbeat` | Set up recurring prompts |
| `/refine` | Refine reusable instructions and memory |
| `/tree`, `/fork`, `/clone` | Navigate history or branch a session |
| `/telegram` | Open bot setup and connection controls |
| `/settings`, `/mcp`, `/reload` | Manage settings, integrations, and resources |

From the source checkout, you can also inspect and control background sessions:

```bash
./prime-agent.sh agents              # Browse active and saved sessions
./prime-agent.sh status              # Inspect background services
./prime-agent.sh doctor              # Diagnose service problems
./prime-agent.sh schedule list       # List scheduled prompts
./prime-agent.sh shutdown            # Stop background services
```

See the [CLI reference](packages/coding-agent/docs/usage.md) for attachment, session selection, scheduling, and automation options.

## Telegram

Run `/telegram` in the terminal and select **Setup with BotFather**. The setup guide walks you through creating a bot, entering its token, and opening a one-time pairing link.

Once paired, your private Telegram chat controls the connected session. Send text or use commands such as `/new`, `/resume`, `/model`, `/effort`, `/compact`, `/goal`, and `/context`. Use `/stop` to interrupt work and `/help` to see the available commands.

The connector runs in the background while the computer and agent service remain running. The initial version supports one paired private account and text messages. Read the [Telegram guide](packages/coding-agent/docs/telegram.md) for connection controls, permissions, storage, and recovery.

## Work That Spans Sessions

Optimus combines a persistent execution environment with durable session state. The agent can keep working after you detach from the terminal, and you can return to its history, goals, and running tasks later.

The recursive language model approach treats context as data the agent can inspect and manipulate in Python. Subagents provide separate execution contexts for independent tasks. Skills package recurring workflows into reusable capabilities.

Compaction helps manage the model's context window. Goals track an objective over time, while heartbeats and schedules bring work back into a session. Autonomous mode can continue within configured budgets and run user-defined checks; reaching a limit does not mean a task is complete.

Refinement records lessons as supplemental state, local to the session by default. It can update reusable instructions and retain a rollback history without rewriting the base system prompt. Executable skill changes still need their own review and validation.

## Documentation

- [Usage and CLI reference](packages/coding-agent/docs/usage.md) — commands, sessions, and output modes
- [Telegram](packages/coding-agent/docs/telegram.md) — bot setup, pairing, and remote session commands
- [Background agents](packages/coding-agent/docs/long-running-agents.md) — detach, reattach, goals, and schedules
- [RLM programming model](packages/coding-agent/docs/rlm.md) — Python execution, subagents, and context management
- [Skills](packages/coding-agent/docs/skills.md) — install and create reusable capabilities
- [MCP integrations](packages/coding-agent/docs/mcp-integrations.md) — connect external tools and services
- [Providers](packages/coding-agent/docs/providers.md) — authentication and model configuration
- [Settings](packages/coding-agent/docs/settings.md) — user and project configuration
- [JSON mode](packages/coding-agent/docs/json.md) and [RPC mode](packages/coding-agent/docs/rpc.md) — headless automation
- [Architecture](packages/coding-agent/docs/architecture.md) — daemon, worker, kernel, and persistence boundaries
- [Development](packages/coding-agent/docs/development.md) — source setup and validation

## Contributing

Development takes place in the [Telemus AI fork](https://github.com/telemusai/prime-agent). Follow [AGENTS.md](AGENTS.md) for repository conventions and validation requirements. Keep pull requests focused, explain the resulting behavior, and include the checks used to verify it.

## Acknowledgements

Optimus is based on **[Prime Agent](https://github.com/PrimeIntellect-ai/prime-agent)** and **[Pi](https://github.com/earendil-works/pi)**. We thank the Prime Intellect team, Mario Zechner, and the maintainers and contributors of both projects for the foundations of this work.

Prime Agent builds on Pi's agent toolkit and terminal interface. Optimus continues that lineage as a fork maintained by Telemus AI. The original copyright notices are retained in [LICENSE](LICENSE).

For the research behind Prime Agent, see [Prime Agent: A Self-Improving RLM Harness](https://arxiv.org/abs/2608.23552), the [RLM overview](https://www.primeintellect.ai/blog/rlm), and [Continual Harness](https://arxiv.org/abs/2605.09998).

<details>
<summary>Prime Agent research citation</summary>

```bibtex
@article{karten2026prime,
  title={Prime Agent: A Self-Improving RLM Harness},
  author={Karten, Seth and Zhang, Alex L. and Thomas, Kevin and Müller, Sebastian and Bakouch, Elie and Auras, Daniel and Senghaas, Mika and Obeid, Fares and Dunas, Konstantin and Hagemann, Johannes and Jaghouar, Sami},
  journal={arXiv preprint arXiv:2608.23552},
  year={2026}
}
```

</details>

## License

Optimus is distributed under the [MIT License](LICENSE).
