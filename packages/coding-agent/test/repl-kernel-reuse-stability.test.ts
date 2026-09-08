import { afterEach, describe, expect, it, vi } from "vitest";
import { ReplKernelManager } from "../src/core/kernel/repl-manager.js";
import { KERNEL_ABORT_GRACE_MS, KERNEL_BUSY_REUSE_WAIT_MS } from "../src/core/kernel/shared.js";

interface Internals {
	state: string;
	activeExecution?: {
		requestId: string;
		settled: boolean;
		stdout: string;
		result?: string;
		error?: { ename: string };
	};
	handleEvent(event: Record<string, unknown>): void;
}

function fixture() {
	const manager = new ReplKernelManager({ cwd: process.cwd() });
	const writes: Record<string, unknown>[] = [];
	const kill = vi.fn(() => true);
	Object.assign(manager, {
		state: "running",
		start: async () => {},
		child: { kill },
		writeLine: async (request: Record<string, unknown>) => {
			writes.push(request);
		},
	});
	return { manager, kernel: manager as unknown as Internals, writes, kill };
}

async function turns() {
	for (let i = 0; i < 30; i++) await Promise.resolve();
}

describe("authoritative REPL reuse barrier", () => {
	afterEach(() => vi.useRealTimers());
	it("runs 100 sequential cells without an interrupt or reuse probe", async () => {
		const { manager, kernel, writes, kill } = fixture();
		try {
			for (let i = 0; i < 100; i++) {
				const execution = manager.execute(`print(${i})`);
				await turns();
				const id = kernel.activeExecution?.requestId;
				kernel.handleEvent({ event: "stdout", id, text: `${i}\n` });
				kernel.handleEvent({ event: "done", id, status: "ok" });
				await expect(execution).resolves.toMatchObject({ status: "ok", stdout: `${i}\n` });
			}
			expect(writes).toHaveLength(100);
			expect(kill).not.toHaveBeenCalled();
		} finally {
			manager.disposeSync();
		}
	});
	it("does not interrupt a long cell merely because it exceeds the reuse deadline", async () => {
		vi.useFakeTimers();
		const { manager, kernel, writes } = fixture();
		try {
			const first = manager.execute("long_operation()");
			const second = manager.execute("next_operation()");
			await turns();
			await vi.advanceTimersByTimeAsync(KERNEL_BUSY_REUSE_WAIT_MS * 2);
			expect(writes).toHaveLength(1);
			kernel.handleEvent({ event: "done", id: kernel.activeExecution?.requestId, status: "ok" });
			await first;
			await turns();
			kernel.handleEvent({ event: "done", id: kernel.activeExecution?.requestId, status: "ok" });
			await expect(second).resolves.toMatchObject({ status: "ok" });
			expect(writes.map((request) => request.type)).toEqual(["execute", "execute"]);
		} finally {
			manager.disposeSync();
		}
	});
	it("requires correlated completion and retains late output beyond three seconds", async () => {
		vi.useFakeTimers();
		const { manager, kernel, writes, kill } = fixture();
		try {
			const abort = new AbortController();
			const first = manager.execute("blocked()", { signal: abort.signal });
			await turns();
			abort.abort();
			await vi.advanceTimersByTimeAsync(KERNEL_ABORT_GRACE_MS);
			await expect(first).resolves.toMatchObject({ status: "aborted" });
			const old = kernel.activeExecution!;
			const next = manager.execute("print('next')");
			await turns();
			const barrier = writes.at(-1)!;
			expect(barrier).toMatchObject({ type: "execute", code: "None" });
			expect(barrier.id).not.toBe(old.requestId);
			kernel.handleEvent({ event: "done", id: "incorrect-parent", status: "ok" });
			await vi.advanceTimersByTimeAsync(4000);
			kernel.handleEvent({ event: "stdout", id: old.requestId, text: "late output" });
			kernel.handleEvent({ event: "result", id: old.requestId, text: "late result" });
			kernel.handleEvent({ event: "error", id: old.requestId, ename: "LateError", evalue: "late", traceback: [] });
			expect(kernel.activeExecution).toBe(old);
			expect(old.stdout).toBe("late output");
			expect(old.error?.ename).toBe("LateError");
			expect(writes.filter((request) => request.type === "interrupt")).toHaveLength(1);
			// Original done is lost; the serialized barrier's done proves it completed.
			kernel.handleEvent({ event: "done", id: barrier.id, status: "ok" });
			await turns();
			expect(kernel.activeExecution?.requestId).not.toBe(old.requestId);
			kernel.handleEvent({ event: "done", id: kernel.activeExecution?.requestId, status: "ok" });
			await expect(next).resolves.toMatchObject({ status: "ok", stdout: "" });
			expect(kill).not.toHaveBeenCalled();
		} finally {
			manager.disposeSync();
		}
	});
	it("cancels a waiting reuse immediately without killing or replaying the active cell", async () => {
		vi.useFakeTimers();
		const { manager, kernel, writes, kill } = fixture();
		try {
			const abort = new AbortController();
			const first = manager.execute("blocked()", { signal: abort.signal });
			await turns();
			abort.abort();
			await vi.advanceTimersByTimeAsync(KERNEL_ABORT_GRACE_MS);
			await first;
			const previous = kernel.activeExecution;
			const waiting = new AbortController();
			const next = manager.execute("must_not_run()", { signal: waiting.signal });
			await turns();
			waiting.abort();
			await expect(next).resolves.toMatchObject({ status: "aborted" });
			expect(kernel.activeExecution).toBe(previous);
			expect(writes.some((request) => request.code === "must_not_run()")).toBe(false);
			expect(kill).not.toHaveBeenCalled();
		} finally {
			manager.disposeSync();
		}
	});
});
