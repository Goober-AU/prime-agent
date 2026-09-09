import { existsSync, mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import lockfile from "proper-lockfile";
import { createCliSubprocessEnv } from "../../cli/subprocess-launch.js";
import { getPackageDir, isBunBinary } from "../../config.js";
import { spawnHidden } from "../../utils/child-process.js";
import { isRecord, type TelegramStore } from "./store.js";

export const TELEGRAM_WORKER_LOCK_OPTIONS = { realpath: false, stale: 10_000, update: 2000 };

export async function telegramWorkerRunning(store: TelegramStore): Promise<boolean> {
	return lockfile.check(store.path("worker"), TELEGRAM_WORKER_LOCK_OPTIONS);
}

export async function withTelegramManagement<T>(store: TelegramStore, action: () => Promise<T>): Promise<T> {
	mkdirSync(store.directory, { recursive: true, mode: 0o700 });
	const release = await lockfile.lock(store.path("management"), {
		...TELEGRAM_WORKER_LOCK_OPTIONS,
		retries: { retries: 50, minTimeout: 100, maxTimeout: 100, factor: 1 },
	});
	try {
		return await action();
	} finally {
		await release();
	}
}

export async function stopTelegramWorker(store: TelegramStore): Promise<void> {
	if (!(await telegramWorkerRunning(store))) return;
	const status = store.status();
	if (!status) throw new Error("Telegram is starting. Retry in a few seconds.");
	store.write("stop.json", { instanceId: status.instanceId });
	for (let attempt = 0; attempt < 150; attempt++) {
		if (!(await telegramWorkerRunning(store))) return;
		await delay(100);
	}
	throw new Error("Telegram has not stopped yet. Retry /telegram pause in a few seconds.");
}

export async function startTelegramWorker(store: TelegramStore): Promise<void> {
	const settings = store.settings();
	if (!settings?.enabled) return;
	if (await telegramWorkerRunning(store)) return;
	if (isBunBinary) throw new Error("Telegram currently requires the Prime Node package installation.");
	const fromSource = import.meta.url.endsWith(".ts");
	const entrypoint = join(
		getPackageDir(),
		fromSource ? "src" : "dist",
		"modes",
		"telegram",
		fromSource ? "worker.ts" : "worker.js",
	);
	if (!existsSync(entrypoint))
		throw new Error("Telegram worker is missing from this installation. Reinstall the current Prime package.");
	const env = createCliSubprocessEnv();
	for (const name of Object.keys(env)) {
		if (
			name.startsWith("PRIME_AGENT_INTERNAL_") ||
			name.startsWith("PRIME_AGENT_SESSION_LEASE") ||
			name === "PRIME_AGENT_ORPHAN_PROCESS_JOURNAL" ||
			name === "PRIME_AGENT_INTERACTIVE_SELF_UPDATE"
		)
			delete env[name];
	}
	rmSync(store.path("stop.json"), { force: true });
	const child = spawnHidden(process.execPath, [...process.execArgv, entrypoint, store.agentDir], {
		cwd: settings.cwd,
		detached: true,
		stdio: "ignore",
		env,
	});
	let failed = false;
	child.once("error", () => {
		failed = true;
	});
	child.once("exit", (code) => {
		if (code !== 0) failed = true;
	});
	child.unref();
	for (let attempt = 0; attempt < 150; attempt++) {
		const status = store.status();
		if (status && status.pid === child.pid && status.phase === "error")
			throw new Error(status.error || "Telegram could not start.");
		if (await telegramWorkerRunning(store)) return;
		if (failed) throw new Error("Telegram worker could not start. Check /telegram status.");
		await delay(100);
	}
	throw new Error("Telegram worker startup timed out. Check /telegram status.");
}

export function telegramStopRequested(store: TelegramStore, instanceId: string): boolean {
	const request = store.read("stop.json");
	return isRecord(request) && request.instanceId === instanceId;
}

export async function describeTelegram(store: TelegramStore): Promise<string> {
	const settings = store.settings();
	if (!settings) return "Telegram is not connected. Use /telegram setup.";
	const status = store.status();
	return [
		`Bot: @${settings.botUsername}`,
		`Connection: ${(await telegramWorkerRunning(store)) ? status?.phase || "starting" : "stopped"}`,
		`Enabled: ${settings.enabled ? "yes" : "no"}`,
		`Paired account: ${settings.pairedUserId ?? "not paired"}`,
		`Session: ${settings.sessionId}`,
		`Directory: ${settings.cwd}`,
		...(status?.error ? [`Last error: ${status.error}`] : []),
		"Use /telegram here to connect this session, /telegram pause to stop, or /telegram disconnect to remove the connection.",
	].join("\n");
}
