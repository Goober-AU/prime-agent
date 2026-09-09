import { randomUUID } from "node:crypto";
import { setTimeout as delay } from "node:timers/promises";
import { pathToFileURL } from "node:url";
import lockfile from "proper-lockfile";
import { EnvHttpProxyAgent, setGlobalDispatcher } from "undici";
import { DaemonAgentConnection } from "../agent-connection/daemon-agent-connection.js";
import { DaemonClient } from "../daemon/daemon-client.js";
import { TelegramApi, TelegramApiError } from "./api.js";
import { acceptTelegramPairing, TelegramBridge } from "./bridge.js";
import { TELEGRAM_WORKER_LOCK_OPTIONS, telegramStopRequested } from "./manager.js";
import { isRecord, type TelegramConnectionSettings, TelegramStore, type TelegramWorkerStatus } from "./store.js";

export async function connectTelegramSession(
	settings: TelegramConnectionSettings,
	agentDir: string,
): Promise<DaemonAgentConnection> {
	const client = new DaemonClient(settings.daemonSocket);
	try {
		await client.connect();
		await client.waitForHello();
		const listing = await client.request({ type: "list", all: true });
		if (!listing.success) throw new Error(listing.error);
		const sessions = isRecord(listing.data) && Array.isArray(listing.data.sessions) ? listing.data.sessions : [];
		const existing = sessions.find((item) => isRecord(item) && item.sessionId === settings.sessionId);
		let activeSessionId =
			isRecord(existing) && typeof existing.activeSessionId === "string" ? existing.activeSessionId : undefined;
		if (!activeSessionId && settings.sessionFile) {
			const created = await client.request({
				type: "create",
				sessionPath: settings.sessionFile,
				config: { cwd: settings.cwd, agentDir },
			});
			if (!created.success) throw new Error(created.error);
			if (isRecord(created.data) && typeof created.data.activeSessionId === "string")
				activeSessionId = created.data.activeSessionId;
		}
		if (!activeSessionId)
			throw new Error("The connected Prime session is unavailable. Open it in Prime and run /telegram here.");
		return await DaemonAgentConnection.attach(client, activeSessionId, {
			closeClientOnDispose: true,
			directTransport: false,
			supportsExtensionUi: true,
			recoverDaemon: async () => {},
		});
	} catch (error) {
		client.close();
		throw error;
	}
}

export async function runTelegramWorker(
	agentDir: string,
	options: { createApi?: (token: string) => TelegramApi; signal?: AbortSignal } = {},
): Promise<void> {
	const store = new TelegramStore(agentDir);
	const controller = new AbortController();
	let release: (() => Promise<void>) | undefined;
	try {
		release = await lockfile.lock(store.path("worker"), {
			...TELEGRAM_WORKER_LOCK_OPTIONS,
			onCompromised: () => controller.abort(),
		});
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ELOCKED") return;
		throw error;
	}
	const status: TelegramWorkerStatus = {
		instanceId: randomUUID(),
		pid: process.pid,
		updatedAt: Date.now(),
		phase: "starting",
	};
	const writeStatus = () => {
		status.updatedAt = Date.now();
		store.write("worker.json", status);
	};
	let connection: DaemonAgentConnection | undefined;
	let bridge: TelegramBridge | undefined;
	let api: TelegramApi | undefined;
	const stop = () => controller.abort();
	options.signal?.addEventListener("abort", stop, { once: true });
	if (options.signal?.aborted) stop();
	process.once("SIGTERM", stop);
	process.once("SIGINT", stop);
	writeStatus();
	const timer = setInterval(() => {
		try {
			if (telegramStopRequested(store, status.instanceId)) stop();
			writeStatus();
		} catch {
			stop();
		}
	}, 1000);
	try {
		const settings = store.settings();
		if (!settings?.enabled) return;
		api = options.createApi?.(settings.botToken) ?? new TelegramApi(settings.botToken);
		const bot = await api.identify(controller.signal);
		if (bot.id !== settings.botId) throw new Error("Telegram bot identity changed. Run /telegram setup again.");
		await api.requirePolling(controller.signal);
		// An unpaired bot must not advertise itself as a UI client to the session.
		const state = store.state(settings.botId);
		if (!settings.pairedUserId) {
			status.phase = "pairing";
			writeStatus();
		}
		while (!settings.pairedUserId && !controller.signal.aborted) {
			try {
				for (const update of await api.updates(state.offset, controller.signal)) {
					if (controller.signal.aborted) return;
					if (update.update_id < state.offset) continue;
					state.offset = update.update_id + 1;
					if (acceptTelegramPairing(settings, update)) {
						store.write("connection.json", settings);
						state.outbox.push({
							id: randomUUID(),
							chatId: settings.pairedUserId!,
							text: "Connected to Prime. This chat controls your connected session. Send a message to begin, or /help for commands.",
						});
					}
					store.write("state.json", state);
					if (settings.pairedUserId) break;
				}
			} catch (error) {
				if (controller.signal.aborted) return;
				if (error instanceof TelegramApiError && (error.code === 401 || error.code === 409)) throw error;
				status.error = api.redact(error);
				writeStatus();
				await delay(
					error instanceof TelegramApiError && error.retryAfter ? error.retryAfter * 1000 : 5000,
					undefined,
					{ signal: controller.signal },
				).catch(() => undefined);
			}
		}
		if (controller.signal.aborted) return;
		connection = await connectTelegramSession(settings, agentDir);
		if (controller.signal.aborted) return;
		status.phase = "running";
		writeStatus();
		bridge = new TelegramBridge(store, settings, api, connection, (error) => {
			if (controller.signal.aborted) return;
			status.error = error;
			writeStatus();
		});
		await bridge.run(controller.signal);
	} catch (error) {
		if (!controller.signal.aborted) {
			status.phase = "error";
			status.error = api?.redact(error) || "Telegram worker could not start.";
		}
	} finally {
		stop();
		clearInterval(timer);
		await connection?.dispose().catch(() => undefined);
		await bridge?.waitForDispatch();
		process.removeListener("SIGTERM", stop);
		process.removeListener("SIGINT", stop);
		options.signal?.removeEventListener("abort", stop);
		if (status.phase !== "error") status.phase = "stopped";
		try {
			writeStatus();
		} finally {
			await release();
		}
	}
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
	setGlobalDispatcher(new EnvHttpProxyAgent());
	const agentDir = process.argv[2];
	if (!agentDir) throw new Error("Telegram worker requires an agent directory.");
	await runTelegramWorker(agentDir);
}
