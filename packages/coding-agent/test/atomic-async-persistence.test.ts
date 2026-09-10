import { mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import type * as FsPromises from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it, vi } from "vitest";

const faults = vi.hoisted(() => ({
	renameCode: "",
	renameRemaining: 0,
	renameCalls: 0,
	directorySyncError: undefined as NodeJS.ErrnoException | undefined,
}));

vi.mock("node:fs/promises", async (importOriginal) => {
	const actual = await importOriginal<typeof FsPromises>();
	return {
		...actual,
		open: async (...args: Parameters<typeof actual.open>) => {
			const handle = await actual.open(...args);
			if (args[1] !== "r" || !faults.directorySyncError) return handle;
			return new Proxy(handle, {
				get(target, property, _receiver) {
					if (property === "sync") return async () => Promise.reject(faults.directorySyncError);
					const value = Reflect.get(target, property, target) as unknown;
					return typeof value === "function" ? value.bind(target) : value;
				},
			});
		},
		rename: async (...args: Parameters<typeof actual.rename>) => {
			faults.renameCalls++;
			if (faults.renameRemaining-- > 0) {
				throw Object.assign(new Error("rename fault"), { code: faults.renameCode, syscall: "rename" });
			}
			return actual.rename(...args);
		},
	};
});

import { AtomicFileWriteCoordinator, writeFileAtomic } from "../src/utils/atomic-file.js";

const tempDirs: string[] = [];

afterEach(() => {
	Object.assign(faults, { renameCode: "", renameRemaining: 0, renameCalls: 0, directorySyncError: undefined });
	for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

function createTempDir(): string {
	const dir = mkdtempSync(join(tmpdir(), "prime-async-atomic-"));
	tempDirs.push(dir);
	return dir;
}

function deferred(): { promise: Promise<void>; resolve: () => void } {
	let resolve = () => {};
	const promise = new Promise<void>((resolvePromise) => {
		resolve = resolvePromise;
	});
	return { promise, resolve };
}

describe("writeFileAtomic", () => {
	it.each(["EPERM", "EACCES", "EBUSY"])("waits asynchronously and recovers from Windows %s", async (code) => {
		const dir = createTempDir();
		const path = join(dir, "state.json");
		writeFileSync(path, "old");
		Object.assign(faults, { renameCode: code, renameRemaining: 2, renameCalls: 0 });
		const delays: number[] = [];
		await writeFileAtomic(path, "new", {
			renameRetry: { platform: "win32", sleep: async (delayMs) => void delays.push(delayMs) },
		});
		expect(delays).toEqual([10, 20]);
		expect(faults.renameCalls).toBe(3);
		expect(readFileSync(path, "utf8")).toBe("new");
		expect(readdirSync(dir)).toEqual(["state.json"]);
	});

	it("propagates unexpected rename errors without retrying or replacing the destination", async () => {
		const dir = createTempDir();
		const path = join(dir, "state.json");
		writeFileSync(path, "old");
		Object.assign(faults, { renameCode: "EIO", renameRemaining: 1, renameCalls: 0 });
		await expect(writeFileAtomic(path, "new", { renameRetry: { platform: "win32" } })).rejects.toThrow(
			"rename fault",
		);
		expect(faults.renameCalls).toBe(1);
		expect(readFileSync(path, "utf8")).toBe("old");
		expect(readdirSync(dir)).toEqual(["state.json"]);
	});

	it("keeps the old destination when interrupted before rename", async () => {
		const dir = createTempDir();
		const path = join(dir, "state.json");
		writeFileSync(path, "old");
		await expect(
			writeFileAtomic(path, "new", {
				fsync: true,
				beforeRename: async () => Promise.reject(new Error("simulated crash before rename")),
			}),
		).rejects.toThrow("simulated crash before rename");
		expect(readFileSync(path, "utf8")).toBe("old");
		expect(readdirSync(dir)).toEqual(["state.json"]);
	});

	it("keeps the renamed destination but reports an unexpected post-rename directory fsync failure", async () => {
		const dir = createTempDir();
		const path = join(dir, "state.json");
		writeFileSync(path, "old");
		faults.directorySyncError = Object.assign(new Error("EIO fsync"), { code: "EIO", syscall: "fsync" });
		await expect(writeFileAtomic(path, "new", { fsync: true, fsyncDir: true })).rejects.toThrow("EIO fsync");
		expect(readFileSync(path, "utf8")).toBe("new");
		expect(readdirSync(dir)).toEqual(["state.json"]);
	});

	it("lets unrelated timers run while a transient rename retry waits", async () => {
		const dir = createTempDir();
		const path = join(dir, "state.json");
		Object.assign(faults, { renameCode: "EPERM", renameRemaining: 1, renameCalls: 0 });
		let timerRan = false;
		const timer = new Promise<void>((resolve) =>
			setTimeout(() => {
				timerRan = true;
				resolve();
			}, 0),
		);
		await writeFileAtomic(path, "new", { renameRetry: { platform: "win32" } });
		expect(timerRan).toBe(true);
		await timer;
	});
});

describe("AtomicFileWriteCoordinator", () => {
	it("serializes generations for one target so an older slow write cannot overwrite a newer write", async () => {
		const dir = createTempDir();
		const path = join(dir, "state.json");
		const gate = deferred();
		const renameEntered = deferred();
		const coordinator = new AtomicFileWriteCoordinator();
		const first = coordinator.write(path, "old-generation", {
			beforeRename: () => {
				renameEntered.resolve();
				return gate.promise;
			},
		});
		const second = coordinator.write(path, "new-generation");
		await renameEntered.promise;
		expect(readdirSync(dir).some((name) => name.endsWith(".tmp"))).toBe(true);
		gate.resolve();
		await expect(Promise.all([first, second])).resolves.toEqual([{ generation: 1 }, { generation: 2 }]);
		expect(readFileSync(path, "utf8")).toBe("new-generation");
	});

	it("allows different targets to make progress concurrently", async () => {
		const dir = createTempDir();
		const firstGate = deferred();
		const secondStarted = deferred();
		const coordinator = new AtomicFileWriteCoordinator();
		const first = coordinator.write(join(dir, "first.json"), "first", { beforeRename: () => firstGate.promise });
		const second = coordinator.write(join(dir, "second.json"), "second", {
			beforeRename: () => void secondStarted.resolve(),
		});
		await secondStarted.promise;
		await second;
		firstGate.resolve();
		await first;
		expect(readFileSync(join(dir, "first.json"), "utf8")).toBe("first");
		expect(readFileSync(join(dir, "second.json"), "utf8")).toBe("second");
	});

	it("serializes Windows lexical case aliases as one target", async () => {
		const gate = deferred();
		const order: string[] = [];
		const coordinator = new AtomicFileWriteCoordinator("win32");
		const first = coordinator.run("C:\\Lease\\Worker.JSON", async () => {
			order.push("first-start");
			await gate.promise;
			order.push("first-end");
		});
		const second = coordinator.run("c:\\lease\\worker.json", () => void order.push("second"));
		await new Promise<void>((resolve) => setImmediate(resolve));
		expect(order).toEqual(["first-start"]);
		gate.resolve();
		await expect(Promise.all([first, second])).resolves.toEqual([{ generation: 1 }, { generation: 2 }]);
		expect(order).toEqual(["first-start", "first-end", "second"]);
	});

	it("keeps case-distinct POSIX targets independent", async () => {
		const firstGate = deferred();
		const secondStarted = deferred();
		const coordinator = new AtomicFileWriteCoordinator("linux");
		const first = coordinator.run("/lease/Worker.json", () => firstGate.promise);
		const second = coordinator.run("/lease/worker.json", () => void secondStarted.resolve());
		await secondStarted.promise;
		await second;
		firstGate.resolve();
		await first;
	});

	it("includes writes admitted while a drain is already waiting", async () => {
		const firstGate = deferred();
		const secondGate = deferred();
		const coordinator = new AtomicFileWriteCoordinator();
		const target = join(createTempDir(), "state.json");
		const first = coordinator.run(target, () => firstGate.promise);
		let drained = false;
		const drain = coordinator.drain(1_000).then(() => {
			drained = true;
		});
		const second = coordinator.run(target, () => secondGate.promise);
		firstGate.resolve();
		await first;
		await new Promise<void>((resolve) => setImmediate(resolve));
		expect(drained).toBe(false);
		secondGate.resolve();
		await second;
		await expect(drain).resolves.toBeUndefined();
	});

	it("bounds drains while preserving a later successful drain", async () => {
		const gate = deferred();
		const coordinator = new AtomicFileWriteCoordinator();
		const write = coordinator.run(join(createTempDir(), "state.json"), () => gate.promise);
		await expect(coordinator.drain(1)).rejects.toThrow("draining atomic file writes");
		gate.resolve();
		await write;
		await expect(coordinator.drain(100)).resolves.toBeUndefined();
	});
});
