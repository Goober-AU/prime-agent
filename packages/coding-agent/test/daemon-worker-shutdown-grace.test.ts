import { afterEach, describe, expect, it, vi } from "vitest";
import { DaemonSupervisor } from "../src/modes/daemon/daemon-supervisor.js";
import * as childProcess from "../src/utils/child-process.js";

const hostPlatform = Object.getOwnPropertyDescriptor(process, "platform")!;

afterEach(() => {
	vi.restoreAllMocks();
	vi.useRealTimers();
	Object.defineProperty(process, "platform", hostPlatform);
});

function createStopFixture() {
	const client = { request: vi.fn(async () => undefined), close: vi.fn() };
	const worker = {
		descriptor: { workerId: "isolated-worker", pid: 987654, processStartId: "test-start", lifecycle: "ready" },
		stopRevision: 0,
		transcriptCaches: new Map(),
		snapshotCache: new Map(),
		client,
	};
	const workers = new Map([[worker.descriptor.workerId, worker]]);
	const cleaned = vi.fn();
	const supervisor = Object.assign(Object.create(DaemonSupervisor.prototype), {
		workers,
		shuttingDown: true,
		persistWorker: vi.fn(),
		processIdentity: vi.fn(() => "current"),
		invalidateWorkerSessionInputPauses: cleaned,
		flipWorkerRosterEntriesInactive: vi.fn(),
	}) as { stopWorkerUntracked(candidate: typeof worker, removeDescriptor: boolean, force: boolean): Promise<void> };
	return {
		client,
		worker,
		workers,
		cleaned,
		stop: (force = false) => supervisor.stopWorkerUntracked(worker, false, force),
	};
}

describe("worker graceful shutdown allowance", () => {
	it.each([
		{ platform: "win32", exitsAfter: 9000 },
		{ platform: "linux", exitsAfter: 1900 },
		{ platform: "darwin", exitsAfter: 1900 },
	])("waits for a cooperative $platform worker without force", async ({ platform, exitsAfter }) => {
		vi.useFakeTimers();
		Object.defineProperty(process, "platform", { value: platform });
		const started = Date.now();
		vi.spyOn(childProcess, "processIdExists").mockImplementation(() => Date.now() - started < exitsAfter);
		const signal = vi.spyOn(childProcess, "signalProcessGroupOrProcess").mockImplementation(() => {});
		const fixture = createStopFixture();
		const stopping = fixture.stop();
		await vi.advanceTimersByTimeAsync(exitsAfter - 25);
		expect(fixture.cleaned).not.toHaveBeenCalled();
		expect(fixture.workers.has(fixture.worker.descriptor.workerId)).toBe(true);
		await vi.advanceTimersByTimeAsync(25);
		await stopping;
		expect(fixture.client.request).toHaveBeenCalledExactlyOnceWith({ type: "shutdown" }, 5000);
		expect(fixture.client.close).toHaveBeenCalledOnce();
		expect(signal).not.toHaveBeenCalled();
		expect(fixture.cleaned).toHaveBeenCalledOnce();
		expect(fixture.workers.size).toBe(0);
		expect(vi.getTimerCount()).toBe(0);
	});

	it.each([
		{ platform: "win32", deadline: 10_000 },
		{ platform: "linux", deadline: 2000 },
		{ platform: "darwin", deadline: 2000 },
	])("keeps a nonresponsive $platform worker registered after $deadline ms", async ({ platform, deadline }) => {
		vi.useFakeTimers();
		Object.defineProperty(process, "platform", { value: platform });
		vi.spyOn(childProcess, "processIdExists").mockReturnValue(true);
		const signal = vi.spyOn(childProcess, "signalProcessGroupOrProcess").mockImplementation(() => {});
		const fixture = createStopFixture();
		let settled = false;
		const stopping = fixture.stop();
		const failed = expect(stopping).rejects.toThrow("Session worker isolated-worker did not stop");
		void stopping.catch(() => {
			settled = true;
		});
		await vi.advanceTimersByTimeAsync(deadline - 25);
		expect(settled).toBe(false);
		await vi.advanceTimersByTimeAsync(25);
		await failed;
		expect(settled).toBe(true);
		expect(signal).not.toHaveBeenCalled();
		expect(fixture.cleaned).not.toHaveBeenCalled();
		expect(fixture.workers.has(fixture.worker.descriptor.workerId)).toBe(true);
		expect(vi.getTimerCount()).toBe(0);
	});

	it.each(["win32", "linux", "darwin"])("keeps the explicit force grace at 500 ms on %s", async (platform) => {
		vi.useFakeTimers();
		Object.defineProperty(process, "platform", { value: platform });
		let alive = true;
		vi.spyOn(childProcess, "processIdExists").mockImplementation(() => alive);
		const signal = vi.spyOn(childProcess, "signalProcessGroupOrProcess").mockImplementation(() => {
			alive = false;
		});
		const fixture = createStopFixture();
		const stopping = fixture.stop(true);
		await vi.advanceTimersByTimeAsync(499);
		expect(signal).not.toHaveBeenCalled();
		await vi.advanceTimersByTimeAsync(1);
		await stopping;
		expect(fixture.client.request).toHaveBeenCalledExactlyOnceWith({ type: "shutdown" }, 1000);
		expect(signal).toHaveBeenCalledExactlyOnceWith(fixture.worker.descriptor.pid, "SIGKILL");
		expect(fixture.workers.size).toBe(0);
		expect(vi.getTimerCount()).toBe(0);
	});
});
