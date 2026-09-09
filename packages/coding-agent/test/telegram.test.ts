import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fauxAssistantMessage, getModel } from "@earendil-works/pi-ai";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AgentConnection, AgentConnectionState } from "../src/modes/agent-connection/types.js";
import {
	privateMessage,
	splitTelegramText,
	TelegramApi,
	TelegramApiError,
	type TelegramUpdate,
} from "../src/modes/telegram/api.js";
import { acceptTelegramPairing, TelegramBridge } from "../src/modes/telegram/bridge.js";
import { parseTelegramCommand, TelegramCommands, telegramCommandMenu } from "../src/modes/telegram/commands.js";
import { describeTelegram, telegramStopRequested, withTelegramManagement } from "../src/modes/telegram/manager.js";
import { createPairing, type TelegramConnectionSettings, TelegramStore } from "../src/modes/telegram/store.js";

const token = "123456:abcdefghijklmnopqrstuvwxyz12345678";
const directories: string[] = [];
afterEach(() => {
	vi.restoreAllMocks();
	for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
});

function fixture(paired = true) {
	const directory = mkdtempSync(join(tmpdir(), "prime-telegram-"));
	directories.push(directory);
	const store = new TelegramStore(directory);
	const settings: TelegramConnectionSettings = {
		version: 1,
		enabled: true,
		botToken: token,
		botId: 123456,
		botUsername: "prime_test_bot",
		daemonSocket: join(directory, "daemon.sock"),
		cwd: directory,
		sessionId: "session-one",
		sessionFile: join(directory, "one.jsonl"),
		...(paired ? { pairedUserId: 42 } : {}),
	};
	store.write("connection.json", settings);
	const state = {
		sessionId: settings.sessionId,
		sessionFile: settings.sessionFile,
		cwd: directory,
		model: getModel("openai-codex", "gpt-5.4"),
		thinkingLevel: "high",
		availableThinkingLevels: ["low", "high"],
		isStreaming: false,
		serviceTier: "default",
	} as AgentConnectionState;
	const fake = {
		getState: vi.fn(async () => state),
		prompt: vi.fn(async () => {}),
		subscribe: vi.fn(() => vi.fn()),
		getCommands: vi.fn(async () => []),
		compact: vi.fn(async () => ({ tokensBefore: 12000 })),
		abortAndClearQueue: vi.fn(async () => {}),
		abortCompaction: vi.fn(async () => {}),
		abortRetry: vi.fn(async () => {}),
		abortBash: vi.fn(async () => {}),
		respondToExtensionUiRequest: vi.fn(async () => {}),
		setModel: vi.fn(async () => {}),
		getAvailableModels: vi.fn(async () => [state.model!]),
		setThinkingLevel: vi.fn(async () => {}),
		setServiceTier: vi.fn(async () => {}),
		newSession: vi.fn(async () => ({ cancelled: false })),
		setSessionName: vi.fn(async () => {}),
		listSavedSessions: vi.fn(async () => [
			{ id: "saved-id", name: "Saved", path: "/saved.jsonl", firstMessage: "hello" },
		]),
		switchSession: vi.fn(async () => ({ cancelled: false })),
	};
	const connection = fake as unknown as AgentConnection;
	const api = new TelegramApi(token, "https://unused.invalid", vi.fn());
	const bridge = new TelegramBridge(store, settings, api, connection);
	return { directory, store, settings, state, fake, connection, api, bridge };
}

function update(id: number, text: string, user = 42, chatType = "private"): TelegramUpdate {
	return {
		update_id: id,
		message: {
			message_id: id + 1,
			date: Math.floor(Date.now() / 1000),
			from: { id: user, is_bot: false },
			chat: { id: user, type: chatType },
			text,
		},
	};
}

function replyText(store: TelegramStore): string {
	return store
		.state(123456)
		.outbox.map((item) => item.text)
		.join("\n");
}

describe("Telegram API and storage", () => {
	it("validates BotFather identity and preserves an existing webhook", async () => {
		const fetcher = vi
			.fn<typeof fetch>()
			.mockResolvedValueOnce(
				Response.json({ ok: true, result: { id: 123456, username: "prime_test_bot", is_bot: true } }),
			)
			.mockResolvedValueOnce(Response.json({ ok: true, result: { url: "https://other.invalid/webhook" } }));
		const api = new TelegramApi(token, "https://example.invalid", fetcher);
		await expect(api.identify()).resolves.toEqual({ id: 123456, username: "prime_test_bot" });
		await expect(api.requirePolling()).rejects.toThrow("already has a webhook");
		expect(fetcher.mock.calls.map(([url]) => String(url).split("/").at(-1))).toEqual(["getMe", "getWebhookInfo"]);
	});

	it("keeps transport errors and Bot API descriptions from exposing the token", async () => {
		const fetcher = vi
			.fn<typeof fetch>()
			.mockRejectedValueOnce(new Error(`failed https://api.telegram.org/bot${token}/getMe`))
			.mockResolvedValueOnce(Response.json({ ok: false, error_code: 401, description: token }, { status: 401 }));
		const api = new TelegramApi(token, "https://example.invalid", fetcher);
		await expect(api.identify()).rejects.toThrow("Cannot reach Telegram");
		await expect(api.identify()).rejects.toThrow("Telegram rejected the bot token");
		expect(api.redact(new Error(token))).toBe("[redacted]");
	});

	it("uses long polling and rejects malformed update IDs", async () => {
		const fetcher = vi
			.fn<typeof fetch>()
			.mockResolvedValueOnce(Response.json({ ok: true, result: [update(0, "hello")] }))
			.mockResolvedValueOnce(Response.json({ ok: true, result: [{ update_id: Number.MAX_SAFE_INTEGER }] }));
		const api = new TelegramApi(token, "https://example.invalid", fetcher);
		await expect(api.updates(0, new AbortController().signal)).resolves.toHaveLength(1);
		expect(JSON.parse(String(fetcher.mock.calls[0][1]?.body))).toEqual({
			offset: 0,
			timeout: 25,
			limit: 50,
			allowed_updates: ["message"],
		});
		await expect(api.updates(1, new AbortController().signal)).rejects.toThrow("invalid updates");
	});

	it("honors API retry metadata without reflecting error bodies", async () => {
		const api = new TelegramApi(
			token,
			"https://example.invalid",
			vi
				.fn<typeof fetch>()
				.mockResolvedValue(
					Response.json(
						{ ok: false, error_code: 429, description: token, parameters: { retry_after: 12 } },
						{ status: 429 },
					),
				),
		);
		await expect(api.send(42, "hello")).rejects.toMatchObject({ code: 429, retryAfter: 12 });
	});

	it("splits long Unicode output without breaking surrogate pairs", () => {
		const text = `${"a".repeat(3999)}${"😀".repeat(3000)}<literal_markdown>`;
		const chunks = splitTelegramText(text);
		expect(chunks.join("")).toBe(text);
		expect(chunks.every((chunk) => chunk.length <= 4000 && Buffer.from(chunk).toString("utf8") === chunk)).toBe(true);
	});

	it("writes private atomic files without touching model or auth settings", async () => {
		const { directory, store } = fixture();
		writeFileSync(join(directory, "auth.json"), "keep auth");
		writeFileSync(join(directory, "models.json"), "keep models");
		const pairing = createPairing();
		store.write("connection.json", { ...store.settings(), pairing: pairing.pairing });
		expect(readFileSync(store.path("connection.json"), "utf8")).not.toContain(pairing.code);
		if (process.platform !== "win32") {
			expect(statSync(store.directory).mode & 0o777).toBe(0o700);
			expect(statSync(store.path("connection.json")).mode & 0o777).toBe(0o600);
		}
		expect(await describeTelegram(store)).not.toContain(token);
		expect(readFileSync(join(directory, "auth.json"), "utf8")).toBe("keep auth");
		expect(readFileSync(join(directory, "models.json"), "utf8")).toBe("keep models");
		store.write("connection.json", { ...store.settings(), pairedUserId: "42" });
		expect(() => store.settings()).toThrow("Invalid Telegram connection settings");
	});

	it("serializes management and addresses stops to the current worker instance", async () => {
		const { store } = fixture();
		const order: string[] = [];
		await Promise.all([
			withTelegramManagement(store, async () => {
				order.push("first");
				await new Promise((resolve) => setTimeout(resolve, 20));
				order.push("done");
			}),
			withTelegramManagement(store, async () => {
				order.push("second");
			}),
		]);
		expect(order).toEqual(["first", "done", "second"]);
		store.write("stop.json", { instanceId: "old" });
		expect(telegramStopRequested(store, "new")).toBe(false);
		expect(telegramStopRequested(store, "old")).toBe(true);
	});
});

describe("Telegram pairing and dispatch", () => {
	it("requires a one-use, unexpired pairing link in a private human chat", async () => {
		const { settings, store, bridge, fake } = fixture(false);
		const pairing = createPairing();
		settings.pairing = pairing.pairing;
		await bridge.accept(update(0, "/start wrong"));
		await bridge.accept(update(1, `/start ${pairing.code}`, 42, "supergroup"));
		await bridge.accept(update(2, `/start@another_bot ${pairing.code}`));
		expect(settings.pairedUserId).toBeUndefined();
		await bridge.accept(update(3, `/start ${pairing.code}`));
		expect(store.settings()?.pairedUserId).toBe(42);
		expect(store.settings()?.pairing).toBeUndefined();
		await bridge.accept(update(4, `/start ${pairing.code}`, 77));
		await bridge.accept(update(5, "steal session", 77));
		expect(fake.prompt).not.toHaveBeenCalled();
		expect(store.settings()?.pairedUserId).toBe(42);
		const expired = fixture(false).settings;
		expired.pairing = createPairing(Date.now() - 700000).pairing;
		expect(acceptTelegramPairing(expired, update(6, `/start ${pairing.code}`))).toBe(false);
	});

	it("rejects bots, mismatched chat identities, and oversized input", () => {
		const bot = update(1, "hello");
		bot.message!.from.is_bot = true;
		const mismatch = update(1, "hello");
		mismatch.message!.chat.id = 77;
		expect(privateMessage(bot)).toBeUndefined();
		expect(privateMessage(mismatch)).toBeUndefined();
		expect(privateMessage(update(1, "x".repeat(17000)))).toBeUndefined();
	});

	it("queues authenticated text using the existing Prime prompt API and deduplicates restarts", async () => {
		const { bridge, store, fake, settings, api, connection } = fixture();
		await bridge.accept(update(0, "hello"));
		await bridge.waitForDispatch();
		expect(fake.prompt).toHaveBeenCalledExactlyOnceWith("hello", {
			source: "interactive",
			queueIfBusy: true,
			streamingBehavior: "followUp",
		});
		const restarted = new TelegramBridge(store, settings, api, connection);
		await restarted.accept(update(0, "hello"));
		await restarted.waitForDispatch();
		expect(fake.prompt).toHaveBeenCalledTimes(1);
		expect(store.state(123456).offset).toBe(1);
	});

	it("keeps /stop responsive during a long compaction and discards queued work", async () => {
		const { bridge, fake, store } = fixture();
		let finish!: () => void;
		fake.compact.mockImplementationOnce(async () => {
			await new Promise<void>((resolve) => {
				finish = resolve;
			});
			return { tokensBefore: 12000 };
		});
		await bridge.accept(update(1, "/compact preserve decisions"));
		await bridge.accept(update(2, "queued work"));
		await bridge.accept(update(3, "/stop"));
		expect(fake.abortCompaction).toHaveBeenCalledOnce();
		expect(store.state(123456).inbox).toEqual([]);
		finish();
		await bridge.waitForDispatch();
		expect(fake.prompt).not.toHaveBeenCalled();
	});

	it("persists undelivered replies and respects retry_after", async () => {
		const { bridge, api, store } = fixture();
		const send = vi.spyOn(api, "send").mockRejectedValueOnce(new TelegramApiError(429, 10));
		bridge.enqueueReply("durable reply");
		await expect(bridge.flushReplies()).rejects.toMatchObject({ code: 429 });
		await bridge.flushReplies();
		expect(send).toHaveBeenCalledTimes(1);
		expect(replyText(store)).toContain("durable reply");
	});

	it("sends public assistant text once, excluding thinking and tool results", async () => {
		const { bridge, store } = fixture();
		const message = fauxAssistantMessage("public answer");
		message.content.unshift({ type: "thinking", thinking: "private reasoning" });
		await bridge.onEvent({ type: "session_event", event: { type: "message_end", message } });
		await bridge.onEvent({ type: "session_event", event: { type: "message_end", message } });
		await bridge.onEvent({
			type: "session_event",
			event: {
				type: "message_end",
				message: {
					role: "toolResult",
					toolCallId: "id",
					toolName: "tool",
					content: [{ type: "text", text: "raw tool output" }],
					isError: false,
					timestamp: Date.now(),
				},
			},
		});
		expect(replyText(store)).toBe("public answer");
	});

	it("accepts each approval only from the paired user and never auto-confirms", async () => {
		const { bridge, store, fake } = fixture();
		await bridge.onEvent({
			type: "extension_ui_request",
			request: {
				id: "approval",
				method: "confirm",
				payload: { title: "Apply change?", message: "Review this change" },
			},
		});
		const id = /\/answer (\w+)/.exec(replyText(store))![1];
		await bridge.accept(update(1, `/answer ${id} yes`, 77));
		await bridge.accept(update(2, `/answer ${id} maybe`));
		expect(fake.respondToExtensionUiRequest).not.toHaveBeenCalled();
		await bridge.accept(update(3, `/answer ${id} no`));
		await bridge.accept(update(4, `/answer ${id} yes`));
		expect(fake.respondToExtensionUiRequest).toHaveBeenCalledExactlyOnceWith("approval", { confirmed: false });
	});

	it("keeps Telegram setup dialogs local and cancels unsupported editors", async () => {
		const { bridge, store, fake } = fixture();
		await bridge.onEvent({
			type: "extension_ui_request",
			request: { id: "token", method: "input", payload: { title: "Telegram bot token" } },
		});
		expect(replyText(store)).toBe("");
		await bridge.onEvent({
			type: "extension_ui_request",
			request: { id: "edit", method: "editor", payload: { title: "Edit" } },
		});
		expect(fake.respondToExtensionUiRequest).toHaveBeenCalledExactlyOnceWith("edit", { cancelled: true });
	});

	it("cleans up subscriptions on startup failure and rejects late state writes", async () => {
		const { bridge, fake, api, store } = fixture();
		vi.spyOn(api, "call").mockRejectedValue(new TelegramApiError(409));
		await expect(bridge.run()).rejects.toMatchObject({ code: 409 });
		expect(fake.subscribe.mock.results[0].value).toHaveBeenCalledOnce();
		await bridge.accept(update(1, "late"));
		bridge.enqueueReply("late");
		expect(store.state(123456).offset).toBe(0);
		expect(replyText(store)).toBe("");
	});

	it("does not replay an uncertain prompt after a crash", async () => {
		const { store, settings, api, connection, fake } = fixture();
		store.write("state.json", { botId: 123456, offset: 2, inbox: [], outbox: [], interruptedUpdate: 1 });
		vi.spyOn(api, "call").mockRejectedValue(new TelegramApiError(409));
		const restarted = new TelegramBridge(store, settings, api, connection);
		await expect(restarted.run()).rejects.toThrow();
		expect(fake.prompt).not.toHaveBeenCalled();
		expect(replyText(store)).toContain("not been sent again automatically");
	});
});

describe("Prime command compatibility", () => {
	it("uses Prime aliases and Telegram menu spelling", () => {
		expect(parseTelegramCommand("/thinking@PRIME_TEST_BOT high", "prime_test_bot")).toEqual({
			name: "effort",
			args: "high",
		});
		expect(parseTelegramCommand("/system_prompt", "prime_test_bot")).toEqual({ name: "system-prompt", args: "" });
		expect(parseTelegramCommand("/clear", "prime_test_bot")).toEqual({ name: "new", args: "" });
		expect(
			telegramCommandMenu().every(
				(item) => /^[a-z0-9_]{1,32}$/.test(item.command) && item.description.length <= 256,
			),
		).toBe(true);
	});

	it("dispatches model, effort, compaction, and resume through session APIs", async () => {
		const { connection, fake } = fixture();
		const reply = vi.fn();
		const commands = new TelegramCommands(connection, reply);
		await commands.execute("model", "openai-codex/gpt-5.4");
		await commands.execute("effort", "high");
		await commands.execute("compact", "preserve decisions");
		await commands.execute("resume", "saved-id");
		await commands.execute("resume", "/custom/session.jsonl");
		expect(fake.setModel).toHaveBeenCalledWith("openai-codex", "gpt-5.4");
		expect(fake.setThinkingLevel).toHaveBeenCalledWith("high");
		expect(fake.compact).toHaveBeenCalledWith("preserve decisions");
		expect(fake.switchSession).toHaveBeenCalledWith("/saved.jsonl");
		expect(fake.switchSession).toHaveBeenCalledWith("/custom/session.jsonl");
		expect(fake.prompt).not.toHaveBeenCalled();
		await expect(commands.execute("effort", "invented")).rejects.toThrow("Available effort levels");
	});

	it("does not send terminal-only or unknown slash commands to the model", async () => {
		const { connection, fake } = fixture();
		const reply = vi.fn();
		const commands = new TelegramCommands(connection, reply);
		await commands.execute("login", "");
		await commands.execute("update", "");
		await commands.execute("telegram", "setup");
		await commands.execute("madeup", "");
		expect(fake.prompt).not.toHaveBeenCalled();
		expect(reply).toHaveBeenCalledTimes(4);
	});
});
