import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ENV_AGENT_DIR } from "../src/config.js";
import { ReplKernelManager } from "../src/core/kernel/repl-manager.js";
import { ORPHAN_PROCESS_JOURNAL_ENV } from "../src/core/orphan-process-journal.js";

// Explicit opt-in: never discover/install into the user's normal kernel environment.
const python = process.env.PRIME_STABILITY_TEST_PYTHON;
describe.skipIf(!python)("real isolated REPL stability", () => {
	let directory: string;
	let manager: ReplKernelManager | undefined;
	const runtimePids: number[] = [];
	beforeEach(() => {
		directory = mkdtempSync(join(tmpdir(), "prime-repl-stability-"));
		vi.stubEnv(ENV_AGENT_DIR, directory);
		vi.stubEnv(ORPHAN_PROCESS_JOURNAL_ENV, join(directory, "orphans.jsonl"));
	});
	afterEach(async () => {
		await manager?.kill();
		try {
			await vi.waitFor(
				() => {
					for (const pid of runtimePids) expect(() => process.kill(pid, 0)).toThrow();
				},
				{ timeout: 5000 },
			);
		} finally {
			runtimePids.length = 0;
			vi.unstubAllEnvs();
			rmSync(directory, { recursive: true, force: true });
		}
	});
	async function recordRuntimePid() {
		const result = await manager!.execute("import os\nprint(os.getpid())");
		expect(result.status).toBe("ok");
		const pid = Number(result.stdout.trim());
		expect(Number.isInteger(pid) && pid > 0).toBe(true);
		runtimePids.push(pid);
		return pid;
	}
	it("snapshots, restarts, restores, reinitializes, and kills the actual Python child", async () => {
		manager = new ReplKernelManager({
			python,
			cwd: directory,
			snapshot: {
				path: join(directory, "state.dill"),
				manifestPath: join(directory, "state.json"),
				debounceMs: 60000,
			},
		});
		await manager.start();
		const firstPid = await recordRuntimePid();
		expect((await manager.execute("value = 42")).status).toBe("ok");
		expect(await manager.snapshotState()).toMatchObject({ saved: expect.arrayContaining(["value"]) });
		await manager.restart();
		await vi.waitFor(() => expect(() => process.kill(firstPid, 0)).toThrow());
		expect((await manager.restoreState())?.restored).toContain("value");
		expect((await manager.execute("print(value)")).stdout.trim()).toBe("42");
		expect(await recordRuntimePid()).not.toBe(firstPid);
		await manager.kill();
	});
	it("cooperatively interrupts and immediately executes another cell", async () => {
		manager = new ReplKernelManager({ python, cwd: directory });
		await manager.start();
		await recordRuntimePid();
		const controller = new AbortController();
		const pending = manager.execute("import asyncio\nprint('ready', flush=True)\nawait asyncio.sleep(60)", {
			signal: controller.signal,
			onStream: (text) => {
				if (text.includes("ready")) controller.abort();
			},
		});
		await expect(pending).resolves.toMatchObject({ status: "aborted" });
		await expect(manager.execute("print('reused')")).resolves.toMatchObject({ status: "ok", stdout: "reused\n" });
	});
});
