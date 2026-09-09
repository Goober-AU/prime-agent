import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createAgentSessionServices } from "../src/core/agent-session-services.js";
import { AuthStorage } from "../src/core/auth-storage.js";
import { createTelegramExtension } from "../src/core/extensions/builtin/telegram.js";
import type { ExtensionAPI, ExtensionCommandContext, RegisteredCommand } from "../src/core/extensions/types.js";
import { SettingsManager } from "../src/core/settings-manager.js";
import * as manager from "../src/modes/telegram/manager.js";
import { TelegramStore } from "../src/modes/telegram/store.js";

const directories: string[] = [];
const token = "123456:abcdefghijklmnopqrstuvwxyz12345678";
afterEach(() => {
	vi.restoreAllMocks();
	vi.unstubAllGlobals();
	for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
});

async function fixture() {
	const directory = mkdtempSync(join(tmpdir(), "prime-telegram-extension-"));
	directories.push(directory);
	const commands = new Map<string, RegisteredCommand>();
	const handlers = new Map<string, (...args: unknown[]) => unknown>();
	const pi = {
		registerCommand: (name: string, command: RegisteredCommand) => commands.set(name, command),
		on: (name: string, handler: (...args: unknown[]) => unknown) => handlers.set(name, handler),
	} as unknown as ExtensionAPI;
	await createTelegramExtension(directory)(pi);
	const ui = { input: vi.fn(async () => token), select: vi.fn(async () => "Setup with BotFather"), notify: vi.fn() };
	const ctx = {
		hasUI: true,
		cwd: directory,
		ui,
		sessionManager: { getSessionId: () => "current-session", getSessionFile: () => join(directory, "session.jsonl") },
	} as unknown as ExtensionCommandContext;
	const start = vi.spyOn(manager, "startTelegramWorker").mockResolvedValue();
	const stop = vi.spyOn(manager, "stopTelegramWorker").mockResolvedValue();
	const fetcher = vi.fn<typeof fetch>().mockImplementation(async (url) =>
		Response.json({
			ok: true,
			result: String(url).endsWith("/getMe")
				? { id: 123456, username: "prime_test_bot", is_bot: true }
				: { url: "" },
		}),
	);
	vi.stubGlobal("fetch", fetcher);
	return { directory, store: new TelegramStore(directory), commands, handlers, ctx, ui, start, stop, fetcher };
}

describe("/telegram terminal setup", () => {
	it("is registered by standard session services and honors --no-extensions", async () => {
		const { directory } = await fixture();
		for (const disabled of [false, true]) {
			const services = await createAgentSessionServices({
				cwd: directory,
				agentDir: directory,
				authStorage: AuthStorage.inMemory(),
				settingsManager: SettingsManager.inMemory(),
				telemetryDisabled: true,
				resourceLoaderOptions: { noExtensions: disabled, noSkills: true, noPromptTemplates: true, noThemes: true },
			});
			const registered = services.resourceLoader
				.getExtensions()
				.extensions.some((extension) => extension.commands.has("telegram"));
			expect(registered).toBe(!disabled);
		}
	});

	it("has no startup side effects before configuration", async () => {
		const { directory, handlers, ctx, start } = await fixture();
		handlers.get("session_start")!({}, ctx);
		await new Promise((resolve) => setImmediate(resolve));
		expect(start).not.toHaveBeenCalled();
		expect(existsSync(join(directory, "telegram"))).toBe(false);
	});

	it("guides BotFather setup, stops the old bridge before requesting credentials, and saves only Telegram files", async () => {
		const { directory, store, commands, ctx, ui, start, stop } = await fixture();
		writeFileSync(join(directory, "models.json"), "model config");
		writeFileSync(join(directory, "auth.json"), "auth config");
		ui.input.mockImplementation(async () => {
			expect(stop).toHaveBeenCalledOnce();
			return token;
		});
		await commands.get("telegram")!.handler("", ctx);
		expect(ui.notify.mock.calls[0][0]).toContain("/newbot");
		expect(ui.notify.mock.calls.some(([message]) => message.includes("https://t.me/prime_test_bot?start="))).toBe(
			true,
		);
		expect(JSON.stringify(ui.notify.mock.calls)).not.toContain(token);
		expect(store.settings()).toMatchObject({ botToken: token, sessionId: "current-session", enabled: true });
		expect(store.settings()?.pairedUserId).toBeUndefined();
		expect(start).toHaveBeenCalledOnce();
		expect(readFileSync(join(directory, "models.json"), "utf8")).toBe("model config");
		expect(readFileSync(join(directory, "auth.json"), "utf8")).toBe("auth config");
	});

	it("does not save an invalid token or overwrite a bot using a webhook", async () => {
		const { commands, ctx, ui, store, fetcher, start } = await fixture();
		ui.input.mockResolvedValueOnce("wrong");
		await commands.get("telegram")!.handler("setup", ctx);
		expect(fetcher).not.toHaveBeenCalled();
		fetcher
			.mockResolvedValueOnce(
				Response.json({ ok: true, result: { id: 123456, username: "prime_test_bot", is_bot: true } }),
			)
			.mockResolvedValueOnce(Response.json({ ok: true, result: { url: "https://other.invalid/webhook" } }));
		await expect(commands.get("telegram")!.handler("setup", ctx)).rejects.toThrow("already has a webhook");
		expect(store.settings()).toBeUndefined();
		expect(start).toHaveBeenCalledTimes(2);
	});

	it("pauses, resumes, and disconnects without deleting unrelated profile data", async () => {
		const { commands, ctx, store, directory, start } = await fixture();
		await commands.get("telegram")!.handler("setup", ctx);
		writeFileSync(join(directory, "auth.json"), "unchanged");
		await commands.get("telegram")!.handler("pause", ctx);
		expect(store.settings()?.enabled).toBe(false);
		expect(start).toHaveBeenCalledTimes(1);
		await commands.get("telegram")!.handler("restart", ctx);
		expect(store.settings()?.enabled).toBe(true);
		await commands.get("telegram")!.handler("disconnect", ctx);
		expect(store.settings()).toBeUndefined();
		expect(readFileSync(join(directory, "auth.json"), "utf8")).toBe("unchanged");
	});

	it("does not replace an already paired account when refreshing a link", async () => {
		const { commands, ctx, store, ui } = await fixture();
		await commands.get("telegram")!.handler("setup", ctx);
		store.write("connection.json", { ...store.settings(), pairedUserId: 42, pairing: undefined });
		await commands.get("telegram")!.handler("pair", ctx);
		expect(store.settings()?.pairedUserId).toBe(42);
		expect(store.settings()?.pairing).toBeUndefined();
		expect(ui.notify.mock.calls.at(-1)?.[0]).toContain("already paired");
	});
});
