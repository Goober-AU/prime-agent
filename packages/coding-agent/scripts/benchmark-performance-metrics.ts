import { mkdir, mkdtemp, readdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { safeRecordPerformanceMetric, type PerformanceMetricEvent } from "@earendil-works/pi-agent-core";
import { LocalPerformanceMetricRecorder } from "../src/core/performance-metrics.js";

interface Options {
	sessions: number;
	requestsPerSession: number;
	trials: number;
	warmups: number;
	output?: string;
}

interface Sample {
	durationMs: number;
	heapDeltaBytes: number;
	writtenBytes: number;
	records: number;
}

function positiveInteger(value: string | undefined, fallback: number): number {
	const parsed = Number.parseInt(value ?? "", 10);
	return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : fallback;
}

function parseArgs(argv: string[]): Options {
	const options: Options = { sessions: 3, requestsPerSession: 200, trials: 7, warmups: 1 };
	for (let index = 0; index < argv.length; index++) {
		const argument = argv[index];
		if (argument === "--sessions") options.sessions = positiveInteger(argv[++index], options.sessions);
		else if (argument === "--requests-per-session") {
			options.requestsPerSession = positiveInteger(argv[++index], options.requestsPerSession);
		} else if (argument === "--trials") options.trials = positiveInteger(argv[++index], options.trials);
		else if (argument === "--warmups") options.warmups = positiveInteger(argv[++index], options.warmups);
		else if (argument === "--output") options.output = argv[++index];
		else if (argument === "--help" || argument === "-h") {
			console.log(
				"Usage: benchmark-performance-metrics.ts [--sessions N] [--requests-per-session N] " +
					"[--trials N] [--warmups N] [--output FILE]",
			);
			process.exit(0);
		} else throw new Error(`Unknown argument: ${argument}`);
	}
	options.sessions = Math.min(options.sessions, 16);
	options.requestsPerSession = Math.max(200, Math.min(options.requestsPerSession, 5000));
	options.trials = Math.min(options.trials, 31);
	options.warmups = Math.min(options.warmups, 5);
	return options;
}

function logicalRequestEvent(session: number, request: number): PerformanceMetricEvent {
	return {
		operation: "logical_request",
		correlation: {
			logicalRequestId: `logical-${session}-${request}`,
			providerAttemptId: `attempt-${session}-${request}`,
		},
		identity: { provider: "synthetic", model: "synthetic-model", api: "synthetic-api", component: "agent" },
		outcome: "success",
		measurements: {
			total_ms: 100,
			wait_ms: 3,
			dispatch_to_response_headers_ms: 40,
			dispatch_to_first_event_ms: 45,
			dispatch_to_first_visible_ms: 50,
			local_gateway_wait_ms: null,
			upstream_wait_ms: null,
			attempt_count: null,
		},
		usage: {
			source: "provider",
			inputTokens: 1000,
			cachedInputTokens: 200,
			outputTokens: 100,
			reasoningTokens: 60,
			totalTokens: 1100,
			cachedInputIncludedInInput: true,
			reasoningIncludedInOutput: true,
		},
	};
}

function providerAttemptEvent(session: number, request: number): PerformanceMetricEvent {
	return {
		operation: "provider_attempt",
		correlation: {
			logicalRequestId: `logical-${session}-${request}`,
			providerAttemptId: `attempt-${session}-${request}`,
		},
		identity: { provider: "synthetic", model: "synthetic-model", api: "synthetic-api", component: "provider" },
		outcome: "success",
		measurements: {
			total_ms: 97,
			dispatch_to_response_headers_ms: 40,
			dispatch_to_first_event_ms: 45,
			dispatch_to_first_visible_ms: 50,
			local_gateway_wait_ms: null,
			upstream_wait_ms: null,
			attempt_count: null,
			attempt_ordinal: 1,
		},
	};
}

async function directoryBytes(path: string): Promise<number> {
	let total = 0;
	for (const entry of await readdir(path, { withFileTypes: true })) {
		if (entry.isFile()) total += (await stat(join(path, entry.name))).size;
	}
	return total;
}

async function runArm(enabled: boolean, root: string, options: Options, label: string): Promise<Sample> {
	const armRoot = join(root, label);
	await mkdir(armRoot, { recursive: true });
	const heapBefore = process.memoryUsage().heapUsed;
	const startedAt = performance.now();
	for (let session = 0; session < options.sessions; session++) {
		const recorder = enabled
			? new LocalPerformanceMetricRecorder({
				directory: armRoot,
				sessionId: `synthetic-session-${session}`,
				randomId: () => `benchmark-${label}-${session}`,
				flushIntervalMs: 60_000,
			})
			: undefined;
		for (let request = 0; request < options.requestsPerSession; request++) {
			const logical = logicalRequestEvent(session, request);
			const attempt = providerAttemptEvent(session, request);
			if (enabled) {
				safeRecordPerformanceMetric(recorder, logical);
				safeRecordPerformanceMetric(recorder, attempt);
			}
		}
		await recorder?.close();
	}
	const durationMs = performance.now() - startedAt;
	const heapDeltaBytes = process.memoryUsage().heapUsed - heapBefore;
	return {
		durationMs,
		heapDeltaBytes,
		writtenBytes: enabled ? await directoryBytes(armRoot) : 0,
		records: options.sessions * options.requestsPerSession * 2,
	};
}

function distribution(samples: Sample[]) {
	const durations = samples.map((sample) => sample.durationMs).sort((left, right) => left - right);
	const middle = Math.floor(durations.length / 2);
	const median =
		durations.length % 2 === 0 ? (durations[middle - 1] + durations[middle]) / 2 : durations[middle];
	return {
		samples: samples.length,
		medianMs: median,
		minMs: durations[0],
		maxMs: durations[durations.length - 1],
		medianMicrosecondsPerRecord: (median * 1000) / samples[0].records,
		heapDeltaBytesRange: {
			min: Math.min(...samples.map((sample) => sample.heapDeltaBytes)),
			max: Math.max(...samples.map((sample) => sample.heapDeltaBytes)),
		},
		writtenBytesRange: {
			min: Math.min(...samples.map((sample) => sample.writtenBytes)),
			max: Math.max(...samples.map((sample) => sample.writtenBytes)),
		},
	};
}

async function main(): Promise<void> {
	const options = parseArgs(process.argv.slice(2));
	const root = await mkdtemp(join(tmpdir(), "prime-performance-metrics-benchmark-"));
	const baseline: Sample[] = [];
	const candidate: Sample[] = [];
	try {
		for (let warmup = 0; warmup < options.warmups; warmup++) {
			await runArm(false, root, options, `warmup-baseline-${warmup}`);
			await runArm(true, root, options, `warmup-candidate-${warmup}`);
		}
		for (let trial = 0; trial < options.trials; trial++) {
			const candidateFirst = trial % 2 === 1;
			if (candidateFirst) {
				candidate.push(await runArm(true, root, options, `candidate-${trial}`));
				baseline.push(await runArm(false, root, options, `baseline-${trial}`));
			} else {
				baseline.push(await runArm(false, root, options, `baseline-${trial}`));
				candidate.push(await runArm(true, root, options, `candidate-${trial}`));
			}
		}
		const report = {
			schemaVersion: 1,
			fixture: {
				syntheticOnly: true,
				providerRequests: 0,
				sessions: options.sessions,
				logicalRequestsPerSession: options.requestsPerSession,
				recordsPerTrial: options.sessions * options.requestsPerSession * 2,
				warmups: options.warmups,
				measuredTrials: options.trials,
				order: "alternating baseline/candidate",
			},
			baseline: distribution(baseline),
			candidate: distribution(candidate),
			overhead: {
				medianMs: distribution(candidate).medianMs - distribution(baseline).medianMs,
				medianMicrosecondsPerRecord:
					((distribution(candidate).medianMs - distribution(baseline).medianMs) * 1000) /
					(options.sessions * options.requestsPerSession * 2),
			},
			notes: [
				"Baseline and candidate construct identical structured events; only candidate records and flushes them.",
				"Ranges are reported instead of p95 because the bounded default has seven measured trials.",
				"Heap deltas include runtime noise and are not peak resident memory.",
				"This benchmark makes no provider request and makes no latency, quality, token-saving, cost, or billing claim.",
			],
		};
		const serialized = `${JSON.stringify(report, null, 2)}\n`;
		if (options.output) {
			const output = resolve(options.output);
			await mkdir(dirname(output), { recursive: true });
			await writeFile(output, serialized, "utf8");
		} else process.stdout.write(serialized);
	} finally {
		await rm(root, { recursive: true, force: true });
	}
}

await main();
