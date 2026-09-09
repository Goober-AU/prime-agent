import { createHash, randomBytes, randomUUID } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";
import type { AgentMessage } from "@earendil-works/pi-agent-core";
import type {
	AgentConnection,
	AgentConnectionEvent,
	AgentConnectionExtensionUiRequest,
	AgentConnectionExtensionUiResponse,
} from "../agent-connection/types.js";
import { privateMessage, splitTelegramText, type TelegramApi, TelegramApiError, type TelegramUpdate } from "./api.js";
import { parseTelegramCommand, TelegramCommands, telegramCommandMenu } from "./commands.js";
import { pairingHash, type TelegramConnectionSettings, type TelegramState, type TelegramStore } from "./store.js";

function messageText(message: AgentMessage): string {
	if (!("content" in message)) return "";
	if (typeof message.content === "string") return message.content;
	return message.content
		.filter((block) => block.type === "text")
		.map((block) => block.text)
		.join("\n");
}

export function acceptTelegramPairing(settings: TelegramConnectionSettings, update: TelegramUpdate): boolean {
	const message = privateMessage(update);
	const command = parseTelegramCommand(message?.text ?? "", settings.botUsername);
	if (
		!message ||
		settings.pairedUserId ||
		command?.name !== "start" ||
		!settings.pairing ||
		Date.now() >= settings.pairing.expiresAt ||
		command.args.length > 64 ||
		pairingHash(command.args) !== settings.pairing.hash
	)
		return false;
	settings.pairedUserId = message.from.id;
	delete settings.pairing;
	return true;
}

interface PendingQuestion {
	request: AgentConnectionExtensionUiRequest;
	expiresAt: number;
}

export class TelegramBridge {
	private readonly state: TelegramState;
	private readonly commands: TelegramCommands;
	private unsubscribe?: () => void;
	private draining?: Promise<void>;
	private sending?: Promise<void>;
	private readonly questions = new Map<string, PendingQuestion>();
	private readonly controller = new AbortController();
	private working = false;
	private nextDeliveryAt = 0;
	private deliveryFailures = 0;

	constructor(
		private readonly store: TelegramStore,
		private readonly settings: TelegramConnectionSettings,
		private readonly api: TelegramApi,
		private readonly connection: AgentConnection,
		private readonly reportError: (error: string | undefined) => void = () => {},
	) {
		this.state = store.state(settings.botId);
		this.commands = new TelegramCommands(connection, (text) => this.enqueueReply(text));
	}

	private save(): void {
		this.store.write("state.json", this.state);
	}

	enqueueReply(text: string): void {
		if (this.controller.signal.aborted || !this.settings.pairedUserId || !text.trim()) return;
		const chunks = splitTelegramText(text);
		if (this.state.outbox.length + chunks.length > 1000)
			throw new Error("Telegram delivery queue is full. Reconnect Telegram before sending more work.");
		for (const chunk of chunks)
			this.state.outbox.push({ id: randomUUID(), chatId: this.settings.pairedUserId, text: chunk });
		this.save();
	}

	async flushReplies(): Promise<void> {
		if (this.sending) return this.sending;
		if (Date.now() < this.nextDeliveryAt) return;
		this.sending = (async () => {
			while (this.state.outbox.length && !this.controller.signal.aborted) {
				const next = this.state.outbox[0];
				if (next.chatId !== this.settings.pairedUserId) {
					this.state.outbox.shift();
					this.save();
					continue;
				}
				await this.api.send(next.chatId, next.text, this.controller.signal);
				if (this.controller.signal.aborted) return;
				this.state.outbox.shift();
				this.save();
				this.deliveryFailures = 0;
				this.reportError(undefined);
				await delay(1100, undefined, { signal: this.controller.signal });
			}
		})()
			.catch((error: unknown) => {
				this.deliveryFailures++;
				this.nextDeliveryAt =
					Date.now() +
					(error instanceof TelegramApiError && error.retryAfter
						? error.retryAfter * 1000
						: Math.min(30_000, 1000 * 2 ** Math.min(this.deliveryFailures - 1, 5)));
				throw error;
			})
			.finally(() => {
				this.sending = undefined;
			});
		return this.sending;
	}

	async accept(update: TelegramUpdate): Promise<void> {
		if (this.controller.signal.aborted) return;
		if (update.update_id < this.state.offset) return;
		const message = privateMessage(update);
		this.state.offset = update.update_id + 1;
		if (!message) {
			this.save();
			return;
		}
		const command = parseTelegramCommand(message.text ?? "", this.settings.botUsername);
		if (!this.settings.pairedUserId) {
			if (acceptTelegramPairing(this.settings, update)) {
				this.store.write("connection.json", this.settings);
				this.enqueueReply(
					"Connected to Prime. This chat controls your connected session. Send a message to begin, or /help for commands.",
				);
			}
			this.save();
			return;
		}
		if (message.from.id !== this.settings.pairedUserId) {
			this.save();
			return;
		}
		if (!message.text) {
			this.enqueueReply(
				"Send a text message to Prime. Photos, files, and voice messages are not supported by this connector yet.",
			);
			this.save();
			return;
		}
		if (command?.name === "ignore") {
			this.save();
			return;
		}
		if (command?.name === "answer") {
			this.save();
			await this.answer(command.args);
			return;
		}
		if (command?.name === "stop" || command?.name === "cancel") {
			this.state.inbox = [];
			this.save();
			await this.commands.execute(command.name, command.args);
			return;
		}
		if (this.state.inbox.length >= 100)
			this.enqueueReply("Prime already has 100 messages waiting. Use /stop to clear the queue.");
		else this.state.inbox.push({ id: update.update_id, text: message.text });
		this.save();
		this.drainInbox();
	}

	private drainInbox(): void {
		if (this.draining || this.controller.signal.aborted) return;
		this.draining = (async () => {
			while (this.state.inbox.length && !this.controller.signal.aborted) {
				const next = this.state.inbox.shift()!;
				// Commit the attempt before dispatch: never replay an uncertain tool-bearing prompt after a crash.
				this.state.interruptedUpdate = next.id;
				this.save();
				try {
					const command = parseTelegramCommand(next.text, this.settings.botUsername);
					if (command) await this.commands.execute(command.name, command.args);
					else
						await this.connection.prompt(next.text, {
							source: "interactive",
							queueIfBusy: true,
							streamingBehavior: "followUp",
						});
					await this.saveBinding();
				} catch (error) {
					this.enqueueReply(this.api.redact(error));
				}
				if (this.controller.signal.aborted) break;
				delete this.state.interruptedUpdate;
				this.save();
			}
		})()
			.catch((error: unknown) => this.reportError(this.api.redact(error)))
			.finally(() => {
				this.draining = undefined;
			});
	}

	private async saveBinding(): Promise<void> {
		const state = await this.connection.getState();
		if (this.controller.signal.aborted) return;
		this.settings.sessionId = state.sessionId;
		this.settings.sessionFile = state.sessionFile;
		this.settings.cwd = state.cwd;
		this.store.write("connection.json", this.settings);
	}

	async onEvent(event: AgentConnectionEvent): Promise<void> {
		if (this.controller.signal.aborted) return;
		if (event.type === "session_replaced" || event.type === "session_resynced") {
			await this.saveBinding();
			return;
		}
		if (event.type === "closed") {
			this.reportError("The Prime session disconnected. Open Prime and run /telegram restart.");
			this.controller.abort();
			return;
		}
		if (!this.settings.pairedUserId) return;
		if (event.type === "extension_ui_request") {
			await this.question(event.request);
			return;
		}
		if (event.type !== "session_event") return;
		const sessionEvent = event.event;
		if (sessionEvent.type === "agent_start") this.working = true;
		if (sessionEvent.type === "agent_end") this.working = false;
		if (sessionEvent.type !== "message_end") return;
		const message = sessionEvent.message;
		if (message.role !== "assistant" && !(message.role === "custom" && message.display)) return;
		const text = messageText(message) || (message.role === "assistant" ? message.errorMessage : undefined);
		if (!text) return;
		const key = createHash("sha256").update(`${this.settings.sessionId}:${message.timestamp}:${text}`).digest("hex");
		if (key === this.state.lastAssistantKey) return;
		this.state.lastAssistantKey = key;
		this.enqueueReply(text);
	}

	private async question(request: AgentConnectionExtensionUiRequest): Promise<void> {
		// Connection management belongs to the invoking terminal, including its credential dialog.
		if (typeof request.payload.title === "string" && request.payload.title.startsWith("Telegram")) return;
		if (request.method === "notify") {
			if (typeof request.payload.message === "string") this.enqueueReply(request.payload.message);
			return;
		}
		if (request.method === "editor") {
			await this.connection.respondToExtensionUiRequest(request.id, { cancelled: true });
			this.enqueueReply("This command needs the Prime terminal editor and was cancelled.");
			return;
		}
		if (!["select", "confirm", "input"].includes(request.method)) return;
		const id = randomBytes(4).toString("hex");
		this.questions.set(id, { request, expiresAt: Date.now() + 5 * 60_000 });
		const options = Array.isArray(request.payload.options)
			? request.payload.options.filter((option): option is string => typeof option === "string")
			: [];
		this.enqueueReply(
			[
				String(request.payload.title ?? "Prime needs input"),
				String(request.payload.message ?? ""),
				...options.map((option, index) => `${index + 1}. ${option}`),
				`Reply /answer ${id} ${request.method === "confirm" ? "yes|no" : request.method === "select" ? "<number>" : "<text>"}.`,
				`Cancel with /answer ${id} cancel. Expires in 5 minutes.`,
			]
				.filter(Boolean)
				.join("\n"),
		);
	}

	private async answer(args: string): Promise<void> {
		const match = /^(\S+)\s+([\s\S]+)$/.exec(args);
		const pending = match ? this.questions.get(match[1]) : undefined;
		if (!match || !pending || pending.expiresAt < Date.now()) {
			this.enqueueReply("That question is missing or expired. Check Prime for its current request.");
			return;
		}
		const value = match[2].trim();
		let response: AgentConnectionExtensionUiResponse;
		if (value === "cancel") response = { cancelled: true };
		else if (pending.request.method === "confirm") {
			if (!/^(yes|no)$/i.test(value)) {
				this.enqueueReply(`Use /answer ${match[1]} yes or no.`);
				return;
			}
			response = { confirmed: value.toLowerCase() === "yes" };
		} else if (pending.request.method === "select") {
			const options = pending.request.payload.options;
			const selected = /^\d+$/.test(value) && Array.isArray(options) ? options[Number(value) - 1] : undefined;
			if (typeof selected !== "string") {
				this.enqueueReply("Choose a number from the question's options.");
				return;
			}
			response = { value: selected };
		} else response = { value };
		this.questions.delete(match[1]);
		await this.connection.respondToExtensionUiRequest(pending.request.id, response);
		this.enqueueReply("Answer sent to Prime.");
	}

	async run(signal?: AbortSignal): Promise<void> {
		const stop = () => this.controller.abort();
		signal?.addEventListener("abort", stop, { once: true });
		if (signal?.aborted) stop();
		let deliveryTimer: ReturnType<typeof setInterval> | undefined;
		let typingTimer: ReturnType<typeof setInterval> | undefined;
		try {
			this.unsubscribe = this.connection.subscribe((event) =>
				this.onEvent(event).catch((error: unknown) => this.reportError(this.api.redact(error))),
			);
			if (this.state.interruptedUpdate !== undefined) {
				this.enqueueReply(
					"Prime restarted while accepting a Telegram message. It has not been sent again automatically. Check /session and /copy before repeating it.",
				);
				delete this.state.interruptedUpdate;
				this.save();
			}
			await this.api.call("setMyCommands", { commands: telegramCommandMenu() }, this.controller.signal);
			this.working = (await this.connection.getState()).isStreaming;
			this.drainInbox();
			deliveryTimer = setInterval(() => {
				void this.flushReplies().catch((error: unknown) => {
					if (!this.controller.signal.aborted) this.reportError(this.api.redact(error));
				});
			}, 1000);
			typingTimer = setInterval(() => {
				if (this.working && this.settings.pairedUserId && this.questions.size === 0)
					void this.api
						.call(
							"sendChatAction",
							{ chat_id: this.settings.pairedUserId, action: "typing" },
							this.controller.signal,
						)
						.catch(() => undefined);
				for (const [id, question] of this.questions)
					if (question.expiresAt <= Date.now()) {
						this.questions.delete(id);
						void this.connection
							.respondToExtensionUiRequest(question.request.id, { cancelled: true })
							.catch(() => undefined);
					}
			}, 4000);
			let failures = 0;
			while (!this.controller.signal.aborted) {
				try {
					const updates = await this.api.updates(this.state.offset, this.controller.signal);
					for (const update of updates) {
						if (this.controller.signal.aborted) break;
						await this.accept(update);
					}
					failures = 0;
					if (!this.deliveryFailures) this.reportError(undefined);
				} catch (error) {
					if (this.controller.signal.aborted) break;
					if (error instanceof TelegramApiError && (error.code === 401 || error.code === 409)) throw error;
					this.reportError(this.api.redact(error));
					await delay(
						error instanceof TelegramApiError && error.retryAfter
							? error.retryAfter * 1000
							: Math.min(30_000, 1000 * 2 ** Math.min(failures++, 5)),
						undefined,
						{ signal: this.controller.signal },
					).catch(() => undefined);
				}
			}
		} finally {
			stop();
			clearInterval(deliveryTimer);
			clearInterval(typingTimer);
			this.unsubscribe?.();
			signal?.removeEventListener("abort", stop);
			for (const question of this.questions.values())
				await this.connection
					.respondToExtensionUiRequest(question.request.id, { cancelled: true })
					.catch(() => undefined);
			this.questions.clear();
			await this.sending?.catch(() => undefined);
		}
	}

	async waitForDispatch(): Promise<void> {
		await this.draining;
	}
}
