import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { describe, expect, it } from "vitest";
import { ENV_AGENT_DIR } from "../src/config.js";
import { DaemonClient } from "../src/modes/daemon/daemon-client.js";
import type { SessionSummary } from "../src/modes/daemon/daemon-session-list.js";
import { TelegramApi, type TelegramUpdate } from "../src/modes/telegram/api.js";
import { stopTelegramWorker, telegramWorkerRunning } from "../src/modes/telegram/manager.js";
import { createPairing, type TelegramConnectionSettings, TelegramStore } from "../src/modes/telegram/store.js";
import { runTelegramWorker } from "../src/modes/telegram/worker.js";

async function until(predicate: () => boolean | Promise<boolean>, description: string): Promise<void> {
	const deadline = Date.now() + 15000;
	while (Date.now() < deadline) {
		if (await predicate()) return;
		await delay(50);
	}
	throw new Error(`Timed out: ${description}`);
}

describe("Telegram with a real Prime daemon", () => {
	it("pairs, exchanges text, switches sessions, and resumes polling after a stop without replay", async () => {
		const directory = mkdtempSync(join(tmpdir(), "prime-telegram-process-"));
		const agentDir = join(directory, "profile");
		const store = new TelegramStore(agentDir);
		const socketPath = join(directory, "daemon.sock");
		const token = "123456:abcdefghijklmnopqrstuvwxyz12345678";
		const updates: TelegramUpdate[] = [];
		const sent: Array<{ chat_id: number; text: string }> = [];
		const offsets: number[] = [];
		const server = createServer(async (request, response) => {
			try {
				const chunks: Buffer[] = [];
				for await (const chunk of request) chunks.push(Buffer.from(chunk));
				const body = JSON.parse(Buffer.concat(chunks).toString("utf8")) as Record<string, unknown>;
				let result: unknown = true;
				switch (request.url?.split("/").at(-1)) {
					case "getMe":
						result = { id: 123456, username: "prime_test_bot", is_bot: true };
						break;
					case "getWebhookInfo":
						result = { url: "" };
						break;
					case "getUpdates":
						offsets.push(Number(body.offset));
						await delay(100);
						result = updates.filter((update) => update.update_id >= Number(body.offset));
						break;
					case "sendMessage":
						sent.push(body as unknown as { chat_id: number; text: string });
						break;
				}
				response.writeHead(200, { "Content-Type": "application/json" });
				response.end(JSON.stringify({ ok: true, result }));
			} catch {
				response.writeHead(500);
				response.end();
			}
		});
		server.listen(0, "127.0.0.1");
		await once(server, "listening");
		const address = server.address();
		if (!address || typeof address === "string") throw new Error("Missing test server port");
		const createApi = (botToken: string) => new TelegramApi(botToken, `http://127.0.0.1:${address.port}`);
		const controller = new AbortController();
		const child = spawn(
			process.execPath,
			[
				resolve(__dirname, "../../../node_modules/tsx/dist/cli.mjs"),
				resolve(__dirname, "../src/cli.ts"),
				"--mode",
				"daemon",
				"--daemon-socket",
				socketPath,
				"--session-dir",
				join(directory, "sessions"),
				"--offline",
			],
			{
				cwd: directory,
				stdio: ["ignore", "pipe", "pipe"],
				env: {
					...process.env,
					[ENV_AGENT_DIR]: agentDir,
					PI_OFFLINE: "1",
					DO_NOT_TRACK: "1",
					PRIME_AGENT_TELEMETRY: "0",
					TSX_TSCONFIG_PATH: resolve(__dirname, "../../../tsconfig.json"),
				},
			},
		);
		let diagnostics = "";
		child.stderr?.on("data", (chunk: Buffer) => {
			diagnostics += chunk.toString();
		});
		child.stdout?.on("data", () => {});
		let client: DaemonClient | undefined;
		let worker: Promise<void> | undefined;
		const add = (id: number, text: string, user = 42) =>
			updates.push({
				update_id: id,
				message: {
					message_id: id + 1,
					date: Math.floor(Date.now() / 1000),
					from: { id: user, is_bot: false },
					chat: { id: user, type: "private" },
					text,
				},
			});
		try {
			await until(async () => {
				if (child.exitCode !== null) throw new Error(`Daemon exited: ${diagnostics}`);
				const candidate = new DaemonClient(socketPath);
				try {
					await candidate.connect(200);
					await candidate.waitForHello(1000);
					client = candidate;
					return true;
				} catch {
					candidate.close();
					return false;
				}
			}, "daemon startup");
			const created = await client!.request({
				type: "create",
				config: {
					cwd: directory,
					agentDir,
					sessionDir: join(directory, "sessions"),
					provider: "telegram-faux",
					model: "test",
					noTools: true,
					noSkills: true,
					noContextFiles: true,
					noPromptTemplates: true,
					telemetryDisabled: true,
					extensions: [resolve(__dirname, "fixtures/telegram-faux-extension.ts")],
				},
			});
			expect(created.success, JSON.stringify(created)).toBe(true);
			if (!created.success) throw new Error(created.error);
			const session = created.data as SessionSummary;
			writeFileSync(join(agentDir, "auth.json"), "{}\n");
			writeFileSync(join(agentDir, "models.json"), '{"providers":{}}\n');
			const pairing = createPairing();
			const settings: TelegramConnectionSettings = {
				version: 1,
				enabled: true,
				botId: 123456,
				botUsername: "prime_test_bot",
				botToken: token,
				daemonSocket: socketPath,
				cwd: directory,
				sessionId: session.sessionId!,
				sessionFile: session.sessionFile,
				pairing: pairing.pairing,
			};
			store.write("connection.json", settings);
			worker = runTelegramWorker(agentDir, { createApi, signal: controller.signal });
			await until(() => store.status()?.phase === "pairing", "waiting for pairing");
			add(0, "unauthorized input", 77);
			await until(() => offsets.includes(1), "discarding unpaired input");
			add(1, `/start ${pairing.code}`);
			await until(() => store.status()?.phase === "running", "paired daemon attachment");
			add(2, "hello from Telegram");
			await until(
				() => sent.some((message) => message.text.includes("Telegram end-to-end response")),
				`assistant response (${store.status()?.error ?? ""})`,
			);
			expect(sent.every((message) => message.chat_id === 42)).toBe(true);
			add(3, "/new --name Telegram-new");
			await until(() => Boolean(store.settings()?.sessionId !== settings.sessionId), "new session binding");
			await until(
				() => sent.some((message) => message.text.includes("Started session")),
				"new session acknowledgement",
			);
			add(4, `/resume ${settings.sessionId}`);
			await until(() => store.settings()?.sessionId === settings.sessionId, "resume original session").catch(
				(error: unknown) => {
					throw new Error(
						`${String(error)}\nReplies: ${JSON.stringify(sent)}\nState: ${JSON.stringify(store.state(123456))}\nStatus: ${JSON.stringify(store.status())}`,
					);
				},
			);
			await until(() => sent.some((message) => message.text.includes("Resumed")), "resume acknowledgement");
			await stopTelegramWorker(store);
			await worker;
			expect(await telegramWorkerRunning(store)).toBe(false);
			worker = runTelegramWorker(agentDir, { createApi, signal: controller.signal });
			await until(() => store.status()?.phase === "running", "worker restart");
			add(5, "hello again");
			await until(
				() =>
					sent.filter((message) => message.text.includes("Telegram end-to-end response")).length === 2 ||
					sent.some((message) => message.text.includes("Second Telegram response")),
				"response after restart",
			);
			const messages = await client!.request({ type: "get_messages", activeSessionId: session.activeSessionId! });
			expect(messages.success).toBe(true);
			if (!messages.success) throw new Error(messages.error);
			const users = (messages.data as { messages: Array<{ role: string; content: unknown }> }).messages.filter(
				(message) => message.role === "user",
			);
			expect(users).toHaveLength(2);
			expect(JSON.stringify(users)).not.toContain("unauthorized input");
			expect(readFileSync(join(agentDir, "auth.json"), "utf8")).toBe("{}\n");
			expect(readFileSync(join(agentDir, "models.json"), "utf8")).toBe('{"providers":{}}\n');
		} finally {
			controller.abort();
			await worker;
			if (client) {
				await client.request({ type: "shutdown" }, 5000).catch(() => undefined);
				client.close();
			}
			if (child.exitCode === null && child.signalCode === null) {
				const exited = once(child, "exit");
				child.kill("SIGTERM");
				await exited;
			}
			server.closeAllConnections();
			await new Promise<void>((resolveClose) => server.close(() => resolveClose()));
			rmSync(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
		}
	}, 60000);
});
