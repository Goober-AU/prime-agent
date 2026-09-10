import type { ChildProcess } from "node:child_process";
import { EventEmitter } from "node:events";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import lockfile from "proper-lockfile";
import { afterEach, describe, expect, it, vi } from "vitest";
import { startTelegramWorker } from "../src/modes/telegram/manager.js";
import { TelegramStore } from "../src/modes/telegram/store.js";
import * as processes from "../src/utils/child-process.js";

const directories: string[] = [];
afterEach(() => {
	vi.restoreAllMocks();
	vi.unstubAllEnvs();
	for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
});

describe("Telegram helper entrypoint contract", () => {
	it("retains the preload guard and passes exactly the selected absolute profile to the helper", async () => {
		const directory = mkdtempSync(join(tmpdir(), "prime-telegram-entry-"));
		directories.push(directory);
		const store = new TelegramStore(directory);
		store.write("connection.json", {
			version: 1,
			enabled: true,
			botId: 123,
			botUsername: "fixture_bot",
			botToken: "123456:abcdefghijklmnopqrstuvwxyz12345678",
			daemonSocket: join(directory, "isolated.sock"),
			cwd: directory,
			sessionId: "fixture-session",
		});
		const guard = `--require=${join(directory, "guard.cjs")}`;
		vi.stubEnv("NODE_OPTIONS", guard);
		vi.stubEnv("PRIME_AGENT_INTERNAL_DAEMON_CATALOG", "1");
		vi.stubEnv("PRIME_AGENT_SESSION_LEASE_TOKEN", "fake-lease");
		vi.spyOn(lockfile, "check").mockResolvedValueOnce(false).mockResolvedValue(true);
		const child = Object.assign(new EventEmitter(), { pid: 123456, unref: vi.fn() });
		const spawn = vi.spyOn(processes, "spawnHidden").mockReturnValue(child as unknown as ChildProcess);
		await startTelegramWorker(store);
		expect(spawn).toHaveBeenCalledOnce();
		const [exe, args, options] = spawn.mock.calls[0];
		expect(exe).toBe(process.execPath);
		expect(args?.slice(0, process.execArgv.length)).toEqual(process.execArgv);
		expect(args).toHaveLength(process.execArgv.length + 2);
		expect(args?.at(-2)?.replaceAll("\\", "/")).toMatch(/\/modes\/telegram\/worker\.(ts|js)$/);
		expect(isAbsolute(args?.at(-2) ?? "")).toBe(true);
		expect(args?.at(-1)).toBe(store.agentDir);
		expect(options).toMatchObject({ cwd: directory, detached: true, stdio: "ignore", env: { NODE_OPTIONS: guard } });
		expect(options?.env?.PRIME_AGENT_INTERNAL_DAEMON_CATALOG).toBeUndefined();
		expect(options?.env?.PRIME_AGENT_SESSION_LEASE_TOKEN).toBeUndefined();
		expect(child.unref).toHaveBeenCalledOnce();
	});
});
