import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { PerformanceMetricEvent, PerformanceMetricRecorder } from "@earendil-works/pi-agent-core";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DaemonSupervisor } from "../src/modes/daemon/daemon-supervisor.js";
import type { DaemonWorkerDescriptor } from "../src/modes/daemon/daemon-worker-protocol.js";
import type { WriteFileAtomicAsyncOptions } from "../src/utils/atomic-file.js";

interface WorkerFixture {
	descriptor: DaemonWorkerDescriptor;
	descriptorPath: string;
	descriptorRetired?: boolean;
}

interface SupervisorInternals {
	descriptorWrites: {
		write(path: string, data: string, options?: WriteFileAtomicAsyncOptions): Promise<{ generation: number }>;
	};
	performanceMetricRecorders: Map<string, PerformanceMetricRecorder | undefined>;
	persistWorker(worker: WorkerFixture): Promise<void>;
	disposePerformanceMetricRecorders(): void;
}

const tempDirs: string[] = [];

afterEach(() => {
	for (const directory of tempDirs.splice(0)) rmSync(directory, { recursive: true, force: true });
});

function worker(directory: string, id: string, rootSessionId?: string): WorkerFixture {
	const now = "2026-09-10T00:00:00.000Z";
	return {
		descriptorPath: join(directory, `${id}.json`),
		descriptor: {
			version: 2,
			workerId: id,
			pid: 123,
			socketPath: join(directory, `${id}.sock`),
			recoveryJournalPath: join(directory, `${id}.recovery.jsonl`),
			supervisorSocketPath: join(directory, "daemon.sock"),
			authenticationToken: "test-token",
			rootActiveSessionId: `${id}-active`,
			...(rootSessionId ? { rootSessionId } : {}),
			createdAt: now,
			updatedAt: now,
			lifecycle: "ready",
			createCommand: { type: "create" },
			consecutiveFailures: 0,
		},
	};
}

function recorder(
	sessionId: string,
	events: PerformanceMetricEvent[],
	clock: () => number = () => 1,
): PerformanceMetricRecorder {
	return {
		sessionId,
		monotonicNow: clock,
		nextId: (scope) => `${scope}-id`,
		record: (event) => events.push(event),
		flush: async () => {},
		close: async () => {},
	};
}

function supervisorWithFactory(
	factory: (options: { agentDir: string; sessionId: string }) => PerformanceMetricRecorder | undefined,
): SupervisorInternals {
	const directory = mkdtempSync(join(tmpdir(), "prime-supervisor-metrics-"));
	tempDirs.push(directory);
	return new DaemonSupervisor(join(directory, "daemon.sock"), {
		defaultSessionConfig: { agentDir: directory, cwd: directory },
		descriptorDir: join(directory, "workers"),
		performanceMetricRecorderFactory: factory,
	}) as unknown as SupervisorInternals;
}

describe("daemon supervisor persistence metrics", () => {
	it("records one terminal path-free event with the locally observed retry count", async () => {
		const events: PerformanceMetricEvent[] = [];
		const times = [10, 15, 20, 28];
		const metricRecorder = recorder("real-session", events, () => times.shift()!);
		const factory = vi.fn(() => metricRecorder);
		const supervisor = supervisorWithFactory(factory);
		let writeNumber = 0;
		supervisor.descriptorWrites.write = vi.fn(async (_path, _data, options) => {
			writeNumber++;
			options?.renameRetry?.onRetry?.({
				attempt: 1,
				delayMs: 10,
				error: Object.assign(new Error("transient"), { code: "EPERM" }),
			});
			if (writeNumber === 1) {
				options?.renameRetry?.onRetry?.({
					attempt: 2,
					delayMs: 20,
					error: Object.assign(new Error("transient"), { code: "EPERM" }),
				});
				return { generation: writeNumber };
			}
			throw new Error("durable write failed");
		});
		const fixture = worker(tempDirs.at(-1)!, "worker", "real-session");

		await supervisor.persistWorker(fixture);
		await expect(supervisor.persistWorker(fixture)).rejects.toThrow("durable write failed");

		expect(factory).toHaveBeenCalledOnce();
		expect(events).toEqual([
			{
				operation: "file_retry",
				identity: { component: "persistence" },
				outcome: "success",
				measurements: { total_ms: 5, retry_count: 2 },
			},
			{
				operation: "file_retry",
				identity: { component: "persistence" },
				outcome: "failure",
				measurements: { total_ms: 8, retry_count: 1 },
			},
		]);
		expect(events.every((event) => event.correlation === undefined)).toBe(true);
		expect(JSON.stringify(events)).not.toContain(tempDirs.at(-1)!);
		expect(JSON.stringify(events)).not.toContain("durable write failed");
	});

	it("leaves unknown-session writes and all telemetry failures outside durability", async () => {
		const factory = vi
			.fn<(options: { agentDir: string; sessionId: string }) => PerformanceMetricRecorder | undefined>()
			.mockImplementationOnce(() => {
				throw new Error("constructor failed");
			})
			.mockReturnValue({
				sessionId: "record-throws",
				monotonicNow: () => {
					throw new Error("clock failed");
				},
				nextId: () => "unused",
				record: () => {
					throw new Error("record failed");
				},
				flush: async () => {},
				close: async () => {},
			});
		const supervisor = supervisorWithFactory(factory);
		supervisor.descriptorWrites.write = vi.fn(async () => ({ generation: 1 }));
		const directory = tempDirs.at(-1)!;

		await supervisor.persistWorker(worker(directory, "unknown"));
		expect(factory).not.toHaveBeenCalled();
		await supervisor.persistWorker(worker(directory, "factory-fails", "factory-fails"));
		await supervisor.persistWorker(worker(directory, "record-throws", "record-throws"));

		expect(factory).toHaveBeenCalledTimes(2);
		expect(supervisor.descriptorWrites.write).toHaveBeenCalledTimes(3);
	});

	it("caps cached recorders at 32 and starts every close without awaiting failures", async () => {
		const closeCalls: Array<ReturnType<typeof vi.fn>> = [];
		const factory = vi.fn(({ sessionId }: { sessionId: string }) => {
			const close =
				sessionId === "session-0"
					? vi.fn(() => {
							throw new Error("sync close failure");
						})
					: sessionId === "session-1"
						? vi.fn(async () => {
								throw new Error("async close failure");
							})
						: vi.fn(async () => {});
			closeCalls.push(close);
			return { ...recorder(sessionId, []), close };
		});
		const supervisor = supervisorWithFactory(factory);
		supervisor.descriptorWrites.write = vi.fn(async () => ({ generation: 1 }));
		const directory = tempDirs.at(-1)!;
		for (let index = 0; index < 33; index++) {
			await supervisor.persistWorker(worker(directory, `worker-${index}`, `session-${index}`));
		}

		expect(factory).toHaveBeenCalledTimes(32);
		expect(supervisor.performanceMetricRecorders.size).toBe(32);
		expect(() => supervisor.disposePerformanceMetricRecorders()).not.toThrow();
		expect(supervisor.performanceMetricRecorders.size).toBe(0);
		expect(closeCalls).toHaveLength(32);
		expect(closeCalls.every((close) => close.mock.calls.length === 1)).toBe(true);
	});
});
