import { mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { PerformanceMetricRecorder } from "@earendil-works/pi-agent-core";
import { getModel } from "@earendil-works/pi-ai";
import { afterEach, describe, expect, it, vi } from "vitest";

const metricFixture = vi.hoisted(() => ({ closeCalls: 0 }));
let closeStartedResolve!: () => void;
const closeStarted = new Promise<void>((resolve) => {
	closeStartedResolve = resolve;
});
const neverClosingRecorder: PerformanceMetricRecorder = {
	sessionId: "sdk-never-close",
	monotonicNow: () => 0,
	nextId: (scope) => `${scope}-1`,
	record: () => {},
	flush: async () => {},
	close: () => {
		metricFixture.closeCalls++;
		closeStartedResolve();
		return new Promise<void>(() => {});
	},
};

vi.mock("../src/core/performance-metrics.js", async (importOriginal) => ({
	...(await importOriginal<typeof import("../src/core/performance-metrics.js")>()),
	createLocalPerformanceMetricRecorderFromEnvironment: () => neverClosingRecorder,
}));

import { createAgentSession } from "../src/core/sdk.js";

describe("SDK performance recorder lifecycle", () => {
	const roots: string[] = [];
	afterEach(() => {
		vi.useRealTimers();
		metricFixture.closeCalls = 0;
		while (roots.length > 0) rmSync(roots.pop()!, { recursive: true, force: true });
	});

	it("passes one session recorder and bounds disposal even when close never settles", async () => {
		const root = join(tmpdir(), `prime-sdk-metric-lifecycle-${Date.now()}-${Math.random()}`);
		roots.push(root);
		const cwd = join(root, "project");
		const agentDir = join(root, "agent");
		mkdirSync(cwd, { recursive: true });
		mkdirSync(agentDir, { recursive: true });
		const model = getModel("anthropic", "claude-sonnet-4-5");

		const { session } = await createAgentSession({ cwd, agentDir, model: model!, tools: [] });
		expect(session.agent.performanceMetrics).toEqual({
			recorder: neverClosingRecorder,
			hostOwnsLogicalRequestTerminal: true,
		});

		vi.useFakeTimers();
		let disposed = false;
		const disposal = session.disposeAsync({ kernelSnapshot: false }).then(() => {
			disposed = true;
		});
		await closeStarted;
		await vi.advanceTimersByTimeAsync(999);
		expect(disposed).toBe(false);
		await vi.advanceTimersByTimeAsync(1);
		await disposal;
		expect(disposed).toBe(true);
		expect(metricFixture.closeCalls).toBe(1);
	});
});
