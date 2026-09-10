import type { ChildProcess } from "node:child_process";
import { EventEmitter } from "node:events";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { PassThrough } from "node:stream";
import { afterEach, describe, expect, it, vi } from "vitest";
import { success } from "../src/modes/daemon/daemon-protocol.js";
import type { SessionSummary } from "../src/modes/daemon/daemon-session-list.js";
import { DaemonSupervisor } from "../src/modes/daemon/daemon-supervisor.js";
import type { DaemonCreateCommand, DaemonWorkerDescriptor } from "../src/modes/daemon/daemon-worker-protocol.js";
import * as childProcesses from "../src/utils/child-process.js";

interface WorkerFixture {
	descriptor: DaemonWorkerDescriptor;
	descriptorPath: string;
	summaries: Map<string, SessionSummary>;
	snapshotCache: Map<string, unknown>;
	transcriptCaches: Map<string, unknown>;
	snapshotGenerations: Map<string, unknown>;
	snapshotLoads: Map<string, unknown>;
	intentionalStop: boolean;
	stopRevision: number;
	descriptorRevision: number;
	launchEnv?: Record<string, string>;
	client?: WorkerClient;
}

interface WorkerClient {
	request(command: DaemonCreateCommand): Promise<ReturnType<typeof success>>;
	requestWorker: ReturnType<typeof vi.fn>;
	close: ReturnType<typeof vi.fn>;
}

interface SupervisorInternals {
	workers: Map<string, WorkerFixture>;
	assertRecoveryAllowed: () => Promise<void>;
	persistWorkerDescriptorTransition: ReturnType<typeof vi.fn>;
	connectWorker: ReturnType<typeof vi.fn>;
	subscribeWorker: ReturnType<typeof vi.fn>;
	refreshWorkerSummaries: ReturnType<typeof vi.fn>;
	writeRosterEntry: ReturnType<typeof vi.fn>;
	broadcastHeartbeatsChanged: ReturnType<typeof vi.fn>;
	recoverWorker: ReturnType<typeof vi.fn>;
	launchWorker(command: DaemonCreateCommand, existing: WorkerFixture): Promise<WorkerFixture>;
	retryWorkerRecovery(worker: WorkerFixture): Promise<void>;
}

function deferred(): { promise: Promise<void>; resolve(): void } {
	let resolve = () => {};
	const promise = new Promise<void>((resolvePromise) => {
		resolve = resolvePromise;
	});
	return { promise, resolve };
}

const directories: string[] = [];
const streams: PassThrough[] = [];

afterEach(() => {
	vi.restoreAllMocks();
	for (const stream of streams.splice(0)) stream.destroy();
	for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
});

function summary(rootActiveSessionId: string): SessionSummary {
	return {
		id: rootActiveSessionId,
		activeSessionId: rootActiveSessionId,
		sessionId: "root-session",
		sessionFile: "/sessions/root.jsonl",
		lifecycle: "live",
		activity: "idle",
		isSessionActive: false,
		cwd: "/work",
		isStreaming: false,
		isCompacting: false,
		attachedClients: 0,
		messageCount: 0,
		sessionActions: { queuedCount: 0, steering: [], followUps: [] },
	};
}

function fakeChild(): ChildProcess {
	const child = new EventEmitter() as ChildProcess;
	const stderr = new PassThrough();
	const startupGate = new PassThrough();
	streams.push(stderr, startupGate);
	Object.assign(child, {
		pid: process.pid,
		stderr,
		stdio: [null, null, stderr, startupGate],
		exitCode: null,
		signalCode: null,
		unref: vi.fn(),
		kill: vi.fn(() => true),
	});
	startupGate.once("close", () => child.emit("close", 0, null));
	setImmediate(() => child.emit("spawn"));
	return child;
}

function fixture(): { supervisor: SupervisorInternals; worker: WorkerFixture; directory: string } {
	const directory = mkdtempSync(join(tmpdir(), "prime-supervisor-launch-generation-"));
	directories.push(directory);
	const supervisor = new DaemonSupervisor(join(directory, "daemon.sock"), {
		defaultSessionConfig: { agentDir: directory, cwd: directory },
		descriptorDir: join(directory, "workers"),
	}) as unknown as SupervisorInternals;
	const now = "2026-09-10T00:00:00.000Z";
	const worker: WorkerFixture = {
		descriptorPath: join(directory, "workers", "worker.json"),
		descriptor: {
			version: 2,
			workerId: "worker",
			pid: process.pid,
			processStartId: "test-process",
			socketPath: join(directory, "worker.sock"),
			recoveryJournalPath: join(directory, "worker.recovery.jsonl"),
			supervisorSocketPath: join(directory, "daemon.sock"),
			authenticationToken: "test-token",
			rootActiveSessionId: "root-active",
			rootSessionId: "root-session",
			createdAt: now,
			updatedAt: now,
			lifecycle: "recovering",
			createCommand: { type: "create" },
			consecutiveFailures: 0,
		},
		summaries: new Map(),
		snapshotCache: new Map(),
		transcriptCaches: new Map(),
		snapshotGenerations: new Map(),
		snapshotLoads: new Map(),
		intentionalStop: false,
		stopRevision: 0,
		descriptorRevision: 0,
		launchEnv: {},
	};
	const rootSummary = summary(worker.descriptor.rootActiveSessionId);
	const client: WorkerClient = {
		request: vi.fn(async () => success(undefined, "create", rootSummary)),
		requestWorker: vi.fn(),
		close: vi.fn(),
	};
	supervisor.workers.set(worker.descriptor.workerId, worker);
	supervisor.persistWorkerDescriptorTransition = vi.fn(async () => {});
	supervisor.connectWorker = vi.fn(async () => {
		worker.client = client;
		return client;
	});
	supervisor.subscribeWorker = vi.fn(async () => {});
	supervisor.refreshWorkerSummaries = vi.fn(async () => {});
	supervisor.writeRosterEntry = vi.fn();
	supervisor.broadcastHeartbeatsChanged = vi.fn();
	supervisor.recoverWorker = vi.fn(async () => {});
	return { supervisor, worker, directory };
}

async function blockRecoveryCheck(
	supervisor: SupervisorInternals,
	checkNumber: number,
): Promise<{ entered: Promise<void>; release(): void }> {
	const gate = deferred();
	const entered = deferred();
	let checks = 0;
	supervisor.assertRecoveryAllowed = vi.fn(async () => {
		checks++;
		if (checks !== checkNumber) return;
		entered.resolve();
		await gate.promise;
	});
	return { entered: entered.promise, release: gate.resolve };
}

describe("daemon worker launch descriptor generations", () => {
	it("does not overwrite recovery that starts while initial descriptor assignment is blocked", async () => {
		const { supervisor, worker } = fixture();
		const gate = await blockRecoveryCheck(supervisor, 4);
		vi.spyOn(childProcesses, "spawnHidden").mockImplementation(() => fakeChild());
		const launch = supervisor.launchWorker({ type: "create", launchEnv: {} }, worker);
		await gate.entered;
		await supervisor.retryWorkerRecovery(worker);
		gate.release();

		await expect(launch).rejects.toMatchObject({ code: "supervisor_generation_stale" });
		expect(worker.descriptor.lifecycle).toBe("recovering");
		expect(worker.descriptorRevision).toBe(1);
	});

	it("rechecks the starting transition after the final recovery admission await", async () => {
		const { supervisor, worker } = fixture();
		const gate = await blockRecoveryCheck(supervisor, 5);
		vi.spyOn(childProcesses, "spawnHidden").mockImplementation(() => fakeChild());
		const launch = supervisor.launchWorker({ type: "create", launchEnv: {} }, worker);
		await gate.entered;
		await supervisor.retryWorkerRecovery(worker);
		gate.release();

		await expect(launch).rejects.toMatchObject({ code: "supervisor_generation_stale" });
		expect(worker.descriptor.lifecycle).toBe("recovering");
		expect(worker.descriptorRevision).toBe(2);
	});
});
