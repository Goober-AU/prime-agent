import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import type { PerformanceMetricEvent, PerformanceMetricRecorder } from "@earendil-works/pi-agent-core";
import { afterEach, describe, expect, it } from "vitest";
import { ReplKernelManager } from "../src/core/kernel/index.js";
import { casSnapshotRootIn, manifestPathIn, snapshotPathIn } from "../src/core/kernel/state-snapshot.js";

const runtimeSource = resolve(__dirname, "..", "..", "..", "prime-agent-runtime", "src");

function resolveReplPython(): string | null {
	const candidates = [
		process.env.PRIME_AGENT_KERNEL_PYTHON,
		resolve(__dirname, "..", "..", "..", "prime-agent-runtime", ".venv", "Scripts", "python.exe"),
		resolve(__dirname, "..", "..", "..", "prime-agent-runtime", ".venv", "bin", "python"),
	].filter((path): path is string => Boolean(path));
	for (const python of candidates) {
		if (!existsSync(python)) continue;
		const probeRoot = mkdtempSync(join(tmpdir(), "prime-agent-snapshot-python-probe-"));
		try {
			const check = spawnSync(python, ["-c", "import rlm.repl, dill"], {
				encoding: "utf8",
				env: scrubbedKernelEnv(probeRoot),
			});
			if (check.status === 0) return python;
		} finally {
			rmSync(probeRoot, { recursive: true, force: true });
		}
	}
	return null;
}

function scrubbedKernelEnv(root: string, pythonPath = runtimeSource): Record<string, string> {
	return {
		PATH: process.env.PATH ?? "",
		...(process.platform === "win32"
			? {
					SystemRoot: process.env.SystemRoot ?? process.env.SYSTEMROOT ?? "C:/Windows",
					WINDIR: process.env.WINDIR ?? process.env.SystemRoot ?? "C:/Windows",
					ComSpec: process.env.ComSpec ?? "C:/Windows/System32/cmd.exe",
					PATHEXT: process.env.PATHEXT ?? ".COM;.EXE;.BAT;.CMD",
				}
			: {}),
		HOME: root,
		USERPROFILE: root,
		APPDATA: join(root, "appdata"),
		LOCALAPPDATA: join(root, "local-appdata"),
		XDG_CONFIG_HOME: join(root, "config"),
		XDG_CACHE_HOME: join(root, "cache"),
		XDG_DATA_HOME: join(root, "data"),
		TEMP: root,
		TMP: root,
		PYTHONPATH: pythonPath,
		PYTHONNOUSERSITE: "1",
		PYTHONDONTWRITEBYTECODE: "1",
		PRIME_AGENT_HOME: join(root, "agent"),
		PRIME_AGENT_SESSION_DIR: join(root, "sessions"),
		PRIME_AGENT_ARTIFACTS_DIR: join(root, "artifacts"),
		PRIME_AGENT_SOCKET: "",
		PRIME_AGENT_DAEMON_SOCKET: "",
		PRIME_AGENT_KERNEL_BRIDGE: "",
		OPENAI_API_KEY: "",
		OPENAI_BASE_URL: "",
		ANTHROPIC_API_KEY: "",
		ANTHROPIC_BASE_URL: "",
		AZURE_OPENAI_API_KEY: "",
		AZURE_OPENAI_ENDPOINT: "",
		GOOGLE_API_KEY: "",
		GEMINI_API_KEY: "",
		GITHUB_TOKEN: "",
		GH_TOKEN: "",
		AWS_ACCESS_KEY_ID: "",
		AWS_SECRET_ACCESS_KEY: "",
		AWS_SESSION_TOKEN: "",
		HTTP_PROXY: "",
		HTTPS_PROXY: "",
		ALL_PROXY: "",
		NO_PROXY: "",
	};
}

class MemoryRecorder implements PerformanceMetricRecorder {
	readonly sessionId = "isolated-kernel-snapshot-test";
	readonly events: PerformanceMetricEvent[] = [];
	private sequence = 0;

	monotonicNow(): number {
		return performance.now();
	}

	nextId(scope: "logical_request" | "provider_attempt"): string {
		return `${scope}-${++this.sequence}`;
	}

	record(event: PerformanceMetricEvent): void {
		this.events.push(event);
	}

	async flush(): Promise<void> {}
	async close(): Promise<void> {}
}

const python = resolveReplPython();
const describeIfKernel = python ? describe : describe.skip;
const cleanupRoots = new Set<string>();

afterEach(() => {
	for (const root of cleanupRoots) rmSync(root, { recursive: true, force: true });
	cleanupRoots.clear();
});

function isolatedRoot(label: string): string {
	const root = mkdtempSync(join(tmpdir(), `prime-agent-${label}-`));
	cleanupRoots.add(root);
	return root;
}

function managerFor(
	root: string,
	options: { format?: "cas-v2"; recorder?: PerformanceMetricRecorder; pythonPath?: string } = {},
): ReplKernelManager {
	return new ReplKernelManager({
		python: python as string,
		cwd: root,
		env: scrubbedKernelEnv(root, options.pythonPath),
		performanceMetrics: options.recorder,
		snapshot: {
			path: snapshotPathIn(root),
			manifestPath: manifestPathIn(root),
			casRootPath: casSnapshotRootIn(root),
			format: options.format,
			debounceMs: 600_000,
		},
	});
}

describeIfKernel("CAS v2 REPL manager bridge", { tags: ["kernel-heavy"] }, () => {
	it("keeps the writer legacy by default for a fresh session", async () => {
		const root = isolatedRoot("snapshot-default-legacy");
		const manager = managerFor(root);
		try {
			await manager.execute("value = {'kept': True}");
			const snapshot = await manager.snapshotState();
			expect(snapshot?.format).toBe("legacy");
			expect(snapshot?.backwardReadable).toBe(true);
			expect(existsSync(snapshotPathIn(root))).toBe(true);
			expect(existsSync(casSnapshotRootIn(root))).toBe(false);
		} finally {
			await manager.shutdown({ snapshot: false, drainHostRequests: true });
		}
	}, 60_000);

	it("runs first ordinary cells with metrics off and metrics on when no snapshot predecessor exists", async () => {
		const offRoot = isolatedRoot("snapshot-no-metrics-predecessor");
		const off = managerFor(offRoot);
		try {
			expect((await off.execute("40 + 2")).result).toBe("42");
		} finally {
			await off.shutdown({ snapshot: false, drainHostRequests: true });
		}

		const onRoot = isolatedRoot("snapshot-metrics-no-predecessor");
		const on = managerFor(onRoot, { recorder: new MemoryRecorder() });
		try {
			expect((await on.execute("20 + 22")).result).toBe("42");
		} finally {
			await on.shutdown({ snapshot: false, drainHostRequests: true });
		}
	}, 60_000);

	it("records numeric-only serialization, disk, queue, and following-cell delay without blocking the next cell", async () => {
		const root = isolatedRoot("snapshot-cas-metrics");
		const recorder = new MemoryRecorder();
		const manager = managerFor(root, { format: "cas-v2", recorder });
		try {
			const setup = await manager.execute(
				[
					"import time",
					"class SlowSnapshot:",
					"    def __reduce__(self):",
					"        time.sleep(0.2)",
					"        return (SlowSnapshot, ())",
					"SECRET_VARIABLE_MARKER = SlowSnapshot()",
				].join("\n"),
			);
			expect(setup.status).toBe("ok");

			const snapshotPromise = manager.snapshotState();
			await new Promise<void>((resolveImmediate) => globalThis.setImmediate(resolveImmediate));
			const followingCell = manager.execute(
				"print('immediate-next-cell', isinstance(SECRET_VARIABLE_MARKER, SlowSnapshot))",
			);
			const [snapshot, next] = await Promise.all([snapshotPromise, followingCell]);
			expect(snapshot?.format).toBe("cas-v2");
			expect(snapshot?.backwardReadable).toBe(false);
			expect(next.status).toBe("ok");
			expect(next.stdout).toContain("immediate-next-cell True");

			const metric = recorder.events.find((event) => event.operation === "snapshot");
			const measurements = metric?.measurements as Record<string, number | null | undefined> | undefined;
			expect(metric?.outcome).toBe("success");
			expect(measurements?.serialization_ms).toBeGreaterThan(0);
			expect(measurements?.serialization_cpu_ms).toBeGreaterThanOrEqual(0);
			expect(measurements?.serialized_bytes).toBeGreaterThan(0);
			expect(measurements?.written_bytes).toBeGreaterThan(0);
			expect(measurements?.queue_ms).toBeGreaterThanOrEqual(0);
			expect(measurements?.next_cell_delay_ms).toBeGreaterThan(0);
			expect(JSON.stringify(metric)).not.toContain("SECRET_VARIABLE_MARKER");
		} finally {
			await manager.shutdown({ snapshot: false, drainHostRequests: true });
		}
	}, 60_000);

	it("continues an existing v2 root, restores current and explicit previous, and exports a legacy gate", async () => {
		const root = isolatedRoot("snapshot-cas-restart");
		const first = managerFor(root, { format: "cas-v2" });
		try {
			await first.execute("value = 'old'");
			expect((await first.snapshotState())?.format).toBe("cas-v2");
			await first.execute("value = 'new'");
			expect((await first.snapshotState())?.format).toBe("cas-v2");
		} finally {
			await first.shutdown({ snapshot: false, drainHostRequests: true });
		}

		const current = managerFor(root);
		try {
			const restore = await current.restoreState();
			expect(restore?.format).toBe("cas-v2");
			expect((await current.execute("value")).result).toBe("'new'");
			const exported = await current.exportStateForLegacyRuntime("current");
			expect(exported?.backwardReadable).toBe(true);
			expect(existsSync(snapshotPathIn(root))).toBe(true);
		} finally {
			await current.shutdown({ snapshot: false, drainHostRequests: true });
		}

		const previous = managerFor(root);
		try {
			const restore = await previous.restoreState({ source: "previous" });
			expect(restore?.rolledBack).toBe(true);
			expect(restore?.unsavedWorkPossible).toBe(true);
			expect((await previous.execute("value")).result).toBe("'old'");
		} finally {
			await previous.shutdown({ snapshot: false, drainHostRequests: true });
		}
	}, 60_000);

	it("surfaces a corrupt committed pointer instead of starting fresh or reading stale legacy", async () => {
		const root = isolatedRoot("snapshot-cas-corrupt");
		const writer = managerFor(root, { format: "cas-v2" });
		try {
			await writer.execute("latest = 42");
			await writer.snapshotState();
		} finally {
			await writer.shutdown({ snapshot: false, drainHostRequests: true });
		}
		writeFileSync(snapshotPathIn(root), "stale legacy must not load");
		const pointerPath = join(casSnapshotRootIn(root), "CURRENT.json");
		const pointer = JSON.parse(readFileSync(pointerPath, "utf8"));
		pointer.current.sha256 = "0".repeat(64);
		writeFileSync(pointerPath, JSON.stringify(pointer));

		const reader = managerFor(root);
		try {
			const restore = await reader.restoreState();
			expect(restore?.restored).toEqual([]);
			expect(restore?.failed[0]?.name).toBe("<snapshot>");
			expect(await reader.listNamespaceNames()).not.toContain("latest");
		} finally {
			await reader.shutdown({ snapshot: false, drainHostRequests: true });
		}
	}, 60_000);

	it("contains recorder exceptions so snapshot and following cells still complete", async () => {
		const root = isolatedRoot("snapshot-recorder-failure");
		const recorder = new MemoryRecorder();
		recorder.monotonicNow = () => {
			throw new Error("metrics clock unavailable");
		};
		recorder.record = () => {
			throw new Error("metrics disk full");
		};
		const manager = managerFor(root, { format: "cas-v2", recorder });
		try {
			await manager.execute("value = 7");
			expect(await manager.snapshotState()).not.toBeNull();
			expect((await manager.execute("value + 1")).result).toBe("8");
		} finally {
			await manager.shutdown({ snapshot: false, drainHostRequests: true });
		}
	}, 60_000);

	it("does not let a protocol-compatible old runtime touch an existing v2 root", async () => {
		const root = isolatedRoot("snapshot-old-runtime");
		const fakeRoot = join(root, "fake-runtime");
		const packageDir = join(fakeRoot, "rlm");
		mkdirSync(packageDir, { recursive: true });
		writeFileSync(join(packageDir, "__init__.py"), "");
		writeFileSync(
			join(packageDir, "repl.py"),
			[
				"import json, sys",
				"print(json.dumps({'event':'ready','protocol':3,'python':sys.version.split()[0]}), flush=True)",
				"for line in sys.stdin:",
				"    req = json.loads(line)",
				"    if req['type'] == 'shutdown':",
				"        print(json.dumps({'event':'done','id':req['id'],'status':'ok'}), flush=True)",
				"        break",
				"    if req['type'] == 'snapshot':",
				"        print(json.dumps({'event':'done','id':req['id'],'status':'ok','saved':[],'skipped':[],'pruned':[],'bytes':5}), flush=True)",
			].join("\n"),
		);
		const manager = managerFor(root, { pythonPath: fakeRoot });
		try {
			await manager.start();
			expect((await manager.snapshotState())?.format).toBe("legacy");
			mkdirSync(casSnapshotRootIn(root));
			const restore = await manager.restoreState();
			expect(restore?.failed[0]).toMatchObject({ name: "<snapshot>" });
		} finally {
			await manager.shutdown({ snapshot: false, drainHostRequests: true });
		}
	}, 60_000);
});
