import { execFile } from "node:child_process";
import { appendFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { afterEach, describe, expect, it } from "vitest";

const tempRoots: string[] = [];
afterEach(() => {
	for (const root of tempRoots.splice(0)) rmSync(root, { recursive: true, force: true });
});

const execFileAsync = promisify(execFile);
const scriptPath = fileURLToPath(new URL("../scripts/summarize-performance-metrics.mjs", import.meta.url));

async function summarize(root: string, extraArguments: string[] = []) {
	const isolatedHome = join(root, "home");
	mkdirSync(isolatedHome, { recursive: true });
	const { stdout } = await execFileAsync(process.execPath, [scriptPath, "--dir", root, "--json", ...extraArguments], {
		cwd: dirname(scriptPath),
		env: {
			HOME: isolatedHome,
			USERPROFILE: isolatedHome,
			TEMP: root,
			TMP: root,
			...(process.env.SystemRoot ? { SystemRoot: process.env.SystemRoot } : {}),
		},
		timeout: 10_000,
		windowsHide: true,
	});
	return JSON.parse(stdout) as {
		files: {
			bytesSelectedAtOpen: number;
			bytesActuallyRead: number;
			grewWhileReading: number;
			truncatedByByteLimit: number;
		};
		records: { truncated: number };
		measurements: {
			byOperation: Record<string, Record<string, Record<string, number | null>>>;
			providerAttemptsByIdentity: Record<string, Record<string, Record<string, number | null>>>;
		};
		usage: {
			provider: {
				records: number;
				fields: Record<string, { available: number; sum: number }>;
				overlap: Record<string, Record<string, number>>;
			};
		};
		notes: string[];
	};
}

describe("summarize-performance-metrics", () => {
	it("separates nested operation timings and counts authoritative provider usage once", async () => {
		const root = mkdtempSync(join(tmpdir(), "prime-metrics-summary-test-"));
		tempRoots.push(root);
		const records = [
			{
				schemaVersion: 1,
				sequence: 1,
				recordedAt: "2026-09-10T00:00:00.000Z",
				operation: "logical_request",
				correlation: { sessionId: "session-1", logicalRequestId: "logical-1" },
				outcome: "success",
				measurements: { total_ms: 120, wait_ms: 10 },
			},
			{
				schemaVersion: 1,
				sequence: 2,
				recordedAt: "2026-09-10T00:00:00.010Z",
				operation: "provider_attempt",
				correlation: { sessionId: "session-1", logicalRequestId: "logical-1" },
				identity: { provider: "openai", model: "gpt-safe", api: "responses" },
				outcome: "retry",
				measurements: { total_ms: 90, dispatch_to_first_event_ms: 20, attempt_ordinal: 1, attempt_count: null },
				usage: {
					source: "provider",
					inputTokens: 100,
					cachedInputTokens: 20,
					outputTokens: 40,
					reasoningTokens: 30,
					totalTokens: 140,
					cachedInputIncludedInInput: true,
					reasoningIncludedInOutput: true,
				},
			},
			{
				schemaVersion: 1,
				sequence: 3,
				recordedAt: "2026-09-10T00:00:00.020Z",
				operation: "provider_attempt",
				correlation: { sessionId: "session-1", logicalRequestId: "logical-1" },
				identity: { provider: "openai", model: "gpt-safe", api: "responses" },
				outcome: "success",
				measurements: { total_ms: 40, dispatch_to_first_event_ms: 8, attempt_ordinal: 2, attempt_count: null },
				usage: {
					source: "provider",
					inputTokens: 50,
					cachedInputTokens: null,
					outputTokens: 10,
					reasoningTokens: null,
					totalTokens: 60,
					cachedInputIncludedInInput: null,
					reasoningIncludedInOutput: null,
				},
			},
			{
				schemaVersion: 1,
				sequence: 4,
				recordedAt: "2026-09-10T00:00:00.030Z",
				operation: "snapshot",
				correlation: { sessionId: "session-1" },
				outcome: "success",
				measurements: { serialization_ms: 9, serialization_cpu_ms: 4, serialized_bytes: 100 },
			},
		];
		writeFileSync(
			join(root, "performance-v1-session-instance.jsonl"),
			`${records.map((record) => JSON.stringify(record)).join("\n")}\n`,
			"utf8",
		);

		const summary = await summarize(root);
		const requestTotal = summary.measurements.byOperation.logical_request.total_ms;
		const attemptTotal = summary.measurements.byOperation.provider_attempt.total_ms;
		expect(requestTotal).toMatchObject({ available: 1, unavailable: 0, mean: 120, min: 120, max: 120 });
		expect(requestTotal).not.toHaveProperty("sum");
		expect(attemptTotal).toMatchObject({ available: 2, unavailable: 0, mean: 65, min: 40, max: 90 });
		expect(attemptTotal).not.toHaveProperty("sum");
		expect(summary.measurements.byOperation.provider_attempt.attempt_count).toMatchObject({
			available: 0,
			unavailable: 2,
		});
		expect(summary.measurements.byOperation.snapshot.serialization_cpu_ms).toMatchObject({
			available: 1,
			mean: 4,
		});
		expect(
			summary.measurements.providerAttemptsByIdentity["identity:openai/gpt-safe/responses"].total_ms,
		).toMatchObject({ available: 2, mean: 65, min: 40, max: 90 });

		expect(summary.usage.provider.records).toBe(2);
		expect(summary.usage.provider.fields).toMatchObject({
			inputTokens: { available: 2, sum: 150 },
			cachedInputTokens: { available: 1, sum: 20 },
			outputTokens: { available: 2, sum: 50 },
			reasoningTokens: { available: 1, sum: 30 },
			totalTokens: { available: 2, sum: 200 },
		});
		expect(summary.usage.provider.overlap.cachedInputIncludedInInput).toMatchObject({ true: 1, unavailable: 1 });
		expect(summary.usage.provider.overlap.reasoningIncludedInOutput).toMatchObject({ true: 1, unavailable: 1 });
		expect((summary as { cost?: unknown }).cost).toBeUndefined();
		expect(summary.notes.join(" ").toLowerCase()).toContain("not a bill");
	});

	it("caps bytes actually consumed while the selected file is growing", async () => {
		const root = mkdtempSync(join(tmpdir(), "prime-metrics-summary-growth-test-"));
		tempRoots.push(root);
		const path = join(root, "performance-v1-growing-instance.jsonl");
		const line = `${JSON.stringify({
			schemaVersion: 1,
			operation: "tool",
			outcome: "success",
			measurements: { total_ms: 1 },
		})}\n`;
		writeFileSync(path, line.repeat(Math.ceil((2 * 1024 * 1024) / Buffer.byteLength(line))), "utf8");
		const appendBatch = line.repeat(32);
		const writer = setInterval(() => appendFileSync(path, appendBatch, "utf8"), 0);
		let summary: Awaited<ReturnType<typeof summarize>>;
		try {
			summary = await summarize(root, ["--max-files", "1", "--max-bytes", "65536", "--max-line-bytes", "2048"]);
		} finally {
			clearInterval(writer);
		}

		expect(summary.files.bytesActuallyRead).toBeLessThanOrEqual(65_536);
		expect(summary.files.bytesSelectedAtOpen).toBeGreaterThan(summary.files.bytesActuallyRead);
		expect(summary.files.truncatedByByteLimit).toBe(1);
		expect(summary.records.truncated).toBe(1);
	});
});
