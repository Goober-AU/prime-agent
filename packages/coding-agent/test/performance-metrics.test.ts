import { Buffer } from "node:buffer";
import type { PerformanceMetricEvent, PerformanceMetricRecordV1 } from "@earendil-works/pi-agent-core";
import { describe, expect, it, vi } from "vitest";
import {
	createLocalPerformanceMetricRecorderFromEnvironment,
	LocalPerformanceMetricRecorder,
	type PerformanceMetricFileIO,
} from "../src/core/performance-metrics.js";

class MemoryFileIO implements PerformanceMetricFileIO {
	readonly files = new Map<string, string>();
	failAppend = false;

	async mkdir(): Promise<void> {}

	async append(path: string, data: string): Promise<void> {
		if (this.failAppend) {
			const error = new Error("synthetic disk full") as Error & { code: string };
			error.code = "ENOSPC";
			throw error;
		}
		this.files.set(path, (this.files.get(path) ?? "") + data);
	}

	async size(path: string): Promise<number | null> {
		const data = this.files.get(path);
		return data === undefined ? null : Buffer.byteLength(data, "utf8");
	}

	async rename(source: string, destination: string): Promise<void> {
		const data = this.files.get(source);
		if (data === undefined) {
			const error = new Error("synthetic missing file") as Error & { code: string };
			error.code = "ENOENT";
			throw error;
		}
		this.files.set(destination, data);
		this.files.delete(source);
	}

	async remove(path: string): Promise<void> {
		this.files.delete(path);
	}
}

class GatedAppendFileIO extends MemoryFileIO {
	appendCalls = 0;
	readonly appendEntered: Promise<void>;
	private resolveAppendEntered!: () => void;
	private readonly appendGate: Promise<void>;
	private releaseAppend!: () => void;

	constructor() {
		super();
		this.appendEntered = new Promise((resolve) => {
			this.resolveAppendEntered = resolve;
		});
		this.appendGate = new Promise((resolve) => {
			this.releaseAppend = resolve;
		});
	}

	release(): void {
		this.releaseAppend();
	}

	override async append(path: string, data: string): Promise<void> {
		this.appendCalls++;
		this.resolveAppendEntered();
		await this.appendGate;
		await super.append(path, data);
	}
}

class NeverSettlingAppendFileIO extends MemoryFileIO {
	appendCalls = 0;
	readonly appendEntered: Promise<void>;
	private resolveAppendEntered!: () => void;

	constructor() {
		super();
		this.appendEntered = new Promise((resolve) => {
			this.resolveAppendEntered = resolve;
		});
	}

	override async append(): Promise<void> {
		this.appendCalls++;
		this.resolveAppendEntered();
		await new Promise<void>(() => {});
	}
}

function event(id: string): PerformanceMetricEvent {
	return {
		operation: "logical_request",
		correlation: { logicalRequestId: id },
		identity: { provider: "openai", model: "gpt-test", api: "openai-responses", component: "agent" },
		outcome: "success",
		measurements: {
			total_ms: 12.5,
			wait_ms: 1.5,
			dispatch_to_first_visible_ms: 5,
			local_gateway_wait_ms: null,
			upstream_wait_ms: null,
			serialization_cpu_ms: 0.75,
		},
		usage: {
			source: "provider",
			inputTokens: 100,
			cachedInputTokens: 20,
			outputTokens: 10,
			reasoningTokens: null,
			totalTokens: 130,
			cachedInputIncludedInInput: false,
			reasoningIncludedInOutput: null,
		},
	};
}

function parseRecords(io: MemoryFileIO): PerformanceMetricRecordV1[] {
	return [...io.files.values()]
		.flatMap((data) => data.trim().split("\n"))
		.filter(Boolean)
		.map((line) => JSON.parse(line) as PerformanceMetricRecordV1);
}

function createRecorder(
	io: MemoryFileIO,
	overrides: Partial<ConstructorParameters<typeof LocalPerformanceMetricRecorder>[0]> = {},
) {
	let wallNow = 1_800_000_000_000;
	return new LocalPerformanceMetricRecorder({
		directory: "C:/isolated/performance-metrics",
		sessionId: "session-1",
		fileIO: io,
		randomId: () => "instance-1",
		wallNow: () => wallNow++,
		monotonicNow: () => 1,
		flushIntervalMs: 60_000,
		...overrides,
	});
}

describe("LocalPerformanceMetricRecorder", () => {
	it("buffers records and writes a versioned, content-free schema", async () => {
		const io = new MemoryFileIO();
		const recorder = createRecorder(io);
		recorder.record(event("request-1"));
		expect(io.files.size).toBe(0);

		await recorder.flush();
		const [record] = parseRecords(io);
		expect(record).toMatchObject({
			schemaVersion: 1,
			sequence: 1,
			operation: "logical_request",
			correlation: { sessionId: "session-1", logicalRequestId: "request-1" },
		});
		expect(record.usage?.cachedInputIncludedInInput).toBe(false);
		expect(record.measurements?.serialization_cpu_ms).toBe(0.75);
		await recorder.close();
	});

	it("drops unknown fields and sanitizes invalid measurements at runtime", async () => {
		const io = new MemoryFileIO();
		const recorder = createRecorder(io);
		recorder.record({
			...event("request-privacy"),
			measurements: { total_ms: -5, secret_measurement: 7 } as PerformanceMetricEvent["measurements"],
			prompt: "TOP_SECRET_PROMPT",
			toolContent: "TOP_SECRET_TOOL_OUTPUT",
			apiKey: "TOP_SECRET_KEY",
		} as PerformanceMetricEvent);
		await recorder.flush();

		const serialized = [...io.files.values()].join("");
		expect(serialized).not.toContain("TOP_SECRET");
		expect(serialized).not.toContain("secret_measurement");
		expect(parseRecords(io)[0].measurements?.total_ms).toBeNull();
		await recorder.close();
	});

	it("reports records dropped by the strict memory bound", async () => {
		const io = new MemoryFileIO();
		const recorder = createRecorder(io, { maxBufferedRecords: 1 });
		recorder.record(event("kept"));
		recorder.record(event("dropped-1"));
		recorder.record(event("dropped-2"));
		await recorder.flush();

		const records = parseRecords(io);
		expect(records).toHaveLength(2);
		expect(records.find((record) => record.operation === "recorder")?.measurements?.dropped_count).toBe(2);
		await recorder.close();
	});

	it("contains disk-full failures and reports the lost batch after recovery", async () => {
		const io = new MemoryFileIO();
		const recorder = createRecorder(io);
		recorder.record(event("lost-on-disk-full"));
		io.failAppend = true;
		await expect(recorder.flush()).resolves.toBeUndefined();
		expect(io.files.size).toBe(0);

		io.failAppend = false;
		await recorder.flush();
		const records = parseRecords(io);
		expect(records).toHaveLength(1);
		expect(records[0]).toMatchObject({
			operation: "recorder",
			outcome: "unavailable",
			measurements: { dropped_count: 1 },
		});
		await recorder.close();
	});

	it("coalesces many flush calls into one flight and drains one follow-up batch", async () => {
		const io = new GatedAppendFileIO();
		const recorder = createRecorder(io);
		recorder.record(event("before-stall"));
		const flight = recorder.flush();
		await io.appendEntered;

		recorder.record(event("during-stall"));
		for (let index = 0; index < 10_000; index++) {
			expect(recorder.flush()).toBe(flight);
		}
		expect(io.appendCalls).toBe(1);

		io.release();
		await flight;
		expect(parseRecords(io).map((record) => record.correlation.logicalRequestId)).toEqual([
			"before-stall",
			"during-stall",
		]);
		expect(io.appendCalls).toBe(2);
		await recorder.close();
	});

	it("bounds close when file I/O never settles and does not queue flush promises", async () => {
		vi.useFakeTimers();
		try {
			const io = new NeverSettlingAppendFileIO();
			const recorder = createRecorder(io, { closeTimeoutMs: 25 });
			recorder.record(event("never-settles"));
			const flight = recorder.flush();
			await io.appendEntered;

			for (let index = 0; index < 10_000; index++) {
				expect(recorder.flush()).toBe(flight);
			}
			const closing = recorder.close();
			let closed = false;
			void closing.then(() => {
				closed = true;
			});

			await vi.advanceTimersByTimeAsync(24);
			expect(closed).toBe(false);
			await vi.advanceTimersByTimeAsync(1);
			await closing;
			expect(closed).toBe(true);
			expect(io.appendCalls).toBe(1);
		} finally {
			vi.useRealTimers();
		}
	});

	it("rotates bounded files instead of growing one log indefinitely", async () => {
		const io = new MemoryFileIO();
		const recorder = createRecorder(io, {
			maxFileBytes: 4096,
			maxRecordBytes: 1024,
			maxBufferedBytes: 3000,
			maxBufferedRecords: 100,
			maxFiles: 2,
		});
		for (let index = 0; index < 4; index++) recorder.record(event(`first-${index}-${"x".repeat(100)}`));
		await recorder.flush();
		for (let index = 0; index < 4; index++) recorder.record(event(`second-${index}-${"x".repeat(100)}`));
		await recorder.flush();

		expect(io.files.has(recorder.logPath)).toBe(true);
		expect(io.files.has(`${recorder.logPath}.1`)).toBe(true);
		for (const data of io.files.values()) expect(Buffer.byteLength(data, "utf8")).toBeLessThanOrEqual(4096);
		await recorder.close();
	});

	it("is disabled unless the coding-agent opt-in is explicit", async () => {
		const io = new MemoryFileIO();
		expect(
			createLocalPerformanceMetricRecorderFromEnvironment({
				agentDir: "C:/isolated/agent",
				sessionId: "session-disabled",
				env: {},
				fileIO: io,
			}),
		).toBeUndefined();
		const enabled = createLocalPerformanceMetricRecorderFromEnvironment({
			agentDir: "C:/isolated/agent",
			sessionId: "session-enabled",
			env: { PRIME_AGENT_PERFORMANCE_METRICS: "true" },
			fileIO: io,
			randomId: () => "instance-enabled",
			flushIntervalMs: 60_000,
		});
		expect(enabled).toBeInstanceOf(LocalPerformanceMetricRecorder);
		await enabled?.close();
	});
});
