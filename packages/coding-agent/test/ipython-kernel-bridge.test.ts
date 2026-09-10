import { mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { PerformanceMetricRecorder } from "@earendil-works/pi-agent-core";
import { afterEach, describe, expect, it, vi } from "vitest";

const captured = vi.hoisted(() => ({ options: [] as Array<Record<string, unknown>> }));

vi.mock("../src/core/kernel/index.js", async (importOriginal) => {
	const actual = await importOriginal<typeof import("../src/core/kernel/index.js")>();
	class FakeReplKernelManager {
		readonly isRunning = true;
		readonly isDefunct = false;

		constructor(options: Record<string, unknown>) {
			captured.options.push(options);
		}

		async start(): Promise<void> {}
		async restoreState(): Promise<undefined> {
			return undefined;
		}
		async execute(): Promise<Record<string, unknown>> {
			return { status: "ok", stdout: "", stderr: "", result: "", attachments: [], diffs: [] };
		}
		async shutdown(): Promise<void> {}
	}
	return {
		...actual,
		ReplKernelManager: FakeReplKernelManager as unknown as typeof actual.ReplKernelManager,
	};
});

import { casSnapshotRootIn, manifestPathIn, snapshotPathIn } from "../src/core/kernel/state-snapshot.js";
import { IpythonKernelProvisioner } from "../src/core/tools/ipython.js";

function recorder(): PerformanceMetricRecorder {
	return {
		sessionId: "session-bridge",
		monotonicNow: () => 0,
		nextId: (scope) => `${scope}-1`,
		record: () => {},
		flush: async () => {},
		close: async () => {},
	};
}

describe("IpythonKernelProvisioner snapshot and metrics bridge", () => {
	const roots: string[] = [];
	afterEach(() => {
		captured.options.length = 0;
		while (roots.length > 0) rmSync(roots.pop()!, { recursive: true, force: true });
	});

	it("passes explicit cas-v2 and the same recorder to the kernel manager", async () => {
		const root = join(tmpdir(), `prime-ipython-bridge-${Date.now()}-${Math.random()}`);
		roots.push(root);
		const snapshotDir = join(root, "artifacts");
		mkdirSync(snapshotDir, { recursive: true });
		const performanceMetrics = recorder();
		const provisioner = new IpythonKernelProvisioner(root, {
			snapshotDir,
			snapshotFormat: "cas-v2",
			performanceMetrics,
		});

		await provisioner.ensure();
		expect(captured.options).toHaveLength(1);
		expect(captured.options[0]).toMatchObject({
			performanceMetrics,
			snapshot: {
				path: snapshotPathIn(snapshotDir),
				manifestPath: manifestPathIn(snapshotDir),
				casRootPath: casSnapshotRootIn(snapshotDir),
				format: "cas-v2",
			},
		});
		await provisioner.dispose({ snapshot: false });
	});

	it("treats any v2 root as prior state while leaving the writer format absent", async () => {
		const root = join(tmpdir(), `prime-ipython-v2-detect-${Date.now()}-${Math.random()}`);
		roots.push(root);
		const snapshotDir = join(root, "artifacts");
		mkdirSync(casSnapshotRootIn(snapshotDir), { recursive: true });
		const onRestore = vi.fn();
		const provisioner = new IpythonKernelProvisioner(root, { snapshotDir, onRestore });

		await provisioner.ensure();
		expect(captured.options[0]?.snapshot).toMatchObject({
			casRootPath: casSnapshotRootIn(snapshotDir),
			format: undefined,
		});
		expect(onRestore).toHaveBeenCalledWith(
			expect.objectContaining({ restored: [], failed: [], path: snapshotPathIn(snapshotDir) }),
		);
		await provisioner.dispose({ snapshot: false });
	});
});
