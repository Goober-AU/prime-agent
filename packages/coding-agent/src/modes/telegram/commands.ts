import type { ThinkingLevel } from "@earendil-works/pi-agent-core";
import { supportsFastMode } from "@earendil-works/pi-ai";
import { formatAgentCronJob, parseHeartbeatCommand } from "../../core/cron-jobs.js";
import { parseNewSessionCommand } from "../../core/new-session-command.js";
import {
	BUILTIN_SLASH_COMMANDS,
	isBuiltinSlashCommandName,
	parseRefineCommandOptions,
	parseSlashCommand,
	resolveBuiltinSlashCommandName,
} from "../../core/slash-commands.js";
import { resolveToCwd } from "../../core/tools/path-utils.js";
import type { AgentConnection } from "../agent-connection/types.js";

const SUPPORTED_COMMANDS = new Set([
	"new",
	"resume",
	"model",
	"effort",
	"fast",
	"name",
	"session",
	"context",
	"compact",
	"refine",
	"goal",
	"autonomous",
	"heartbeat",
	"heartbeats",
	"rlm-max-depth",
	"reload",
	"system-prompt",
	"copy",
	"settings",
	"fork",
	"tree",
]);

export function telegramCommandMenu(): Array<{ command: string; description: string }> {
	const descriptions: Record<string, string> = {
		settings: "Show current session settings",
		model: "List available models or select one with /model provider/model-id",
		effort: "Show or set the reasoning level",
		copy: "Show the last assistant response",
		resume: "List saved sessions or resume one by ID",
		heartbeats: "List configured heartbeats",
		tree: "List branch entries or switch to an entry ID",
	};
	return [
		{ command: "help", description: "Show Prime commands available in Telegram" },
		{ command: "stop", description: "Interrupt the current turn and clear queued messages" },
		...BUILTIN_SLASH_COMMANDS.filter((command) => SUPPORTED_COMMANDS.has(command.name)).map((command) => ({
			command: command.name.replaceAll("-", "_"),
			description: (descriptions[command.name] ?? command.description).slice(0, 100),
		})),
	];
}

export function telegramHelp(): string {
	return [
		"Prime in Telegram",
		"",
		...telegramCommandMenu().map((command) => `/${command.command} — ${command.description}`),
		"",
		"Aliases such as /clear, /thinking, /rename, and /usage also work. Use /answer for a pending question.",
		"Terminal display, login, package, and update commands stay in the Prime terminal. Text messages are queued as follow-ups while Prime is working.",
	].join("\n");
}

export function parseTelegramCommand(text: string, botUsername: string): { name: string; args: string } | undefined {
	const parsed = parseSlashCommand(text);
	if (!parsed) return undefined;
	const [command, addressedBot, ...extra] = parsed.name.split("@");
	if (extra.length || (addressedBot && addressedBot.toLowerCase() !== botUsername.toLowerCase()))
		return { name: "ignore", args: "" };
	const hyphenated = command.replaceAll("_", "-");
	const normalized = isBuiltinSlashCommandName(hyphenated) ? hyphenated : command;
	return { name: resolveBuiltinSlashCommandName(normalized), args: parsed.args };
}

export class TelegramCommands {
	constructor(
		private readonly connection: AgentConnection,
		private readonly reply: (text: string) => void,
	) {}

	async execute(name: string, args: string): Promise<void> {
		const connection = this.connection;
		switch (name) {
			case "start":
			case "help":
				this.reply(telegramHelp());
				return;
			case "ignore":
				return;
			case "stop":
			case "cancel":
				await connection.abortAndClearQueue();
				await Promise.all([connection.abortCompaction(), connection.abortRetry(), connection.abortBash()]);
				this.reply("Interrupted the current turn and cleared queued messages.");
				return;
			case "settings":
			case "session":
			case "status": {
				const state = await connection.getState();
				this.reply(
					`${state.sessionName || "Prime session"}\nID: ${state.sessionId}\nDirectory: ${state.cwd}\nModel: ${state.model ? `${state.model.provider}/${state.model.id}` : "not selected"}\nEffort: ${state.thinkingLevel}\nFast: ${state.serviceTier === "priority" ? "on" : "off"}\nState: ${state.isStreaming ? "working" : state.isCompacting ? "compacting" : "idle"}`,
				);
				return;
			}
			case "context": {
				const stats = await connection.getSessionStats();
				this.reply(
					`Messages: ${stats.totalMessages}\nTool calls: ${stats.toolCalls}\nTokens: ${stats.tokens.total.toLocaleString()}\nCost: $${stats.cost.toFixed(4)}${stats.contextUsage ? `\nContext: ${JSON.stringify(stats.contextUsage)}` : ""}`,
				);
				return;
			}
			case "model": {
				const models = await connection.getAvailableModels();
				const matches = models.filter((model) => `${model.provider}/${model.id}` === args || model.id === args);
				if (args && matches.length === 1) {
					await connection.setModel(matches[0].provider, matches[0].id);
					this.reply(`Model: ${matches[0].provider}/${matches[0].id}`);
				} else {
					const filtered = args
						? models.filter((model) => `${model.provider}/${model.id}`.toLowerCase().includes(args.toLowerCase()))
						: models;
					this.reply(
						[
							"Use /model <provider/model-id>:",
							...filtered.slice(0, 60).map((model) => `${model.provider}/${model.id}`),
							...(filtered.length > 60 ? ["Use /model <search> to narrow the list."] : []),
						].join("\n"),
					);
				}
				return;
			}
			case "effort": {
				const state = await connection.getState();
				if (!args) {
					this.reply(`Effort: ${state.thinkingLevel}\nUse /effort ${state.availableThinkingLevels.join("|")}`);
					return;
				}
				if (!state.availableThinkingLevels.includes(args as ThinkingLevel))
					throw new Error(`Available effort levels: ${state.availableThinkingLevels.join(", ")}`);
				await connection.setThinkingLevel(args as ThinkingLevel);
				this.reply(`Effort: ${args}`);
				return;
			}
			case "fast": {
				const state = await connection.getState();
				if (!state.model || !supportsFastMode(state.model))
					throw new Error("The current model does not support fast mode.");
				const enable = args === "on" || (!args && state.serviceTier !== "priority");
				if (args && args !== "on" && args !== "off") throw new Error("Usage: /fast [on|off]");
				await connection.setServiceTier(enable ? "priority" : "default");
				this.reply(`Fast mode: ${enable ? "on" : "off"}`);
				return;
			}
			case "name":
				if (args) await connection.setSessionName(args);
				this.reply(`Session name: ${(await connection.getState()).sessionName || "unnamed"}`);
				return;
			case "new": {
				const options = parseNewSessionCommand(args ? ` ${args}` : "");
				if ((await connection.newSession()).cancelled) {
					this.reply("New session cancelled.");
					return;
				}
				if (options.name) await connection.setSessionName(options.name);
				this.reply(`Started session ${(await connection.getState()).sessionId}.`);
				if (options.prompt)
					await connection.prompt(options.prompt, {
						source: "interactive",
						queueIfBusy: true,
						streamingBehavior: "followUp",
					});
				return;
			}
			case "resume": {
				if (args.endsWith(".jsonl")) {
					const path = resolveToCwd(args, (await connection.getState()).cwd);
					const result = await connection.switchSession(path);
					this.reply(result.cancelled ? "Resume cancelled." : `Resumed ${path}.`);
					return;
				}
				const sessions = await connection.listSavedSessions("all");
				const exact = sessions.filter(
					(session) => session.id === args || session.path === args || session.name === args,
				);
				const matches = exact.length ? exact : sessions.filter((session) => args && session.id.startsWith(args));
				if (args && matches.length === 1) {
					const result = await connection.switchSession(matches[0].path);
					this.reply(result.cancelled ? "Resume cancelled." : `Resumed ${matches[0].name || matches[0].id}.`);
				} else
					this.reply(
						[
							"Use /resume <session-id>:",
							...sessions
								.slice(0, 30)
								.map(
									(session) =>
										`${session.id} — ${session.name || session.firstMessage.slice(0, 80) || "untitled"}`,
								),
						].join("\n"),
					);
				return;
			}
			case "compact": {
				this.reply("Compacting context…");
				const result = await connection.compact(args || undefined);
				this.reply(`Context compacted (${result.tokensBefore.toLocaleString()} tokens before compaction).`);
				return;
			}
			case "refine":
				this.reply("Refining session harness…");
				await connection.refine(parseRefineCommandOptions(args));
				this.reply("Refinement completed.");
				return;
			case "goal":
			case "autonomous":
				if (/[\r\n\u2028\u2029]/u.test(args)) throw new Error(`/${name} requires a single-line command.`);
				await connection.prompt(`/${name}${args ? ` ${args}` : ""}`, {
					source: "interactive",
					queueIfBusy: true,
					streamingBehavior: "followUp",
				});
				return;
			case "heartbeat": {
				const command = parseHeartbeatCommand(args);
				if (command.type === "status") {
					const job = await connection.getHeartbeat();
					this.reply(job ? formatAgentCronJob(job) : "No heartbeat is configured.");
				} else if (command.type === "set") {
					this.reply(
						formatAgentCronJob(
							await connection.setHeartbeat(command.schedule, command.instruction, command.deliveryMode),
						),
					);
				} else {
					const job = await connection.updateHeartbeat(command.type);
					this.reply(job ? formatAgentCronJob(job) : "Heartbeat cleared.");
				}
				return;
			}
			case "heartbeats":
				this.reply(
					(await connection.listHeartbeats()).map(({ job }) => formatAgentCronJob(job)).join("\n") ||
						"No heartbeats.",
				);
				return;
			case "rlm-max-depth":
				if (args) {
					if (!/^\d+$/.test(args)) throw new Error("Usage: /rlm_max_depth [non-negative integer]");
					await connection.setRlmMaxDepth(Number(args));
				}
				this.reply(`RLM max depth: ${(await connection.getRlmMaxDepthStatus()).maxDepth}`);
				return;
			case "reload":
				await connection.reload();
				this.reply("Reloaded Prime resources.");
				return;
			case "copy":
				this.reply((await connection.getLastAssistantText()) || "No assistant response yet.");
				return;
			case "system-prompt":
				this.reply(await connection.getSystemPrompt());
				return;
			case "fork": {
				const messages = await connection.getUserMessagesForForking();
				if (!args) {
					this.reply(
						[
							"Use /fork <entry-id>:",
							...messages.slice(-20).map((message) => `${message.entryId} — ${message.text.slice(0, 100)}`),
						].join("\n"),
					);
					return;
				}
				if (!messages.some((message) => message.entryId === args))
					throw new Error("Choose an entry ID from /fork.");
				this.reply((await connection.fork(args)).cancelled ? "Fork cancelled." : "Created session fork.");
				return;
			}
			case "tree": {
				if (args) {
					this.reply(
						(await connection.navigateTree(args)).cancelled
							? "Navigation cancelled."
							: "Switched session branch.",
					);
					return;
				}
				const tree = await connection.getSessionTree();
				this.reply(
					`Current entry: ${tree.leafId || "root"}\nUse /tree <entry-id>. Recent user entries:\n${(
						await connection.getUserMessagesForForking()
					)
						.slice(-20)
						.map((message) => `${message.entryId} — ${message.text.slice(0, 100)}`)
						.join("\n")}`,
				);
				return;
			}
			case "telegram":
				this.reply("Configure the Telegram connection with /telegram in the Prime terminal.");
				return;
			default: {
				if (isBuiltinSlashCommandName(name)) {
					this.reply(`/${name} needs the Prime terminal. Use /help for Telegram commands.`);
					return;
				}
				const commands = await connection.getCommands();
				if (commands.some((command) => command.name === name || command.registeredName === name)) {
					await connection.prompt(`/${name}${args ? ` ${args}` : ""}`, {
						source: "interactive",
						queueIfBusy: true,
						streamingBehavior: "followUp",
					});
				} else this.reply(`Unknown command /${name}. Use /help.`);
			}
		}
	}
}
