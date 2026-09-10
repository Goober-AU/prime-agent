import { existsSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { getModel } from "@earendil-works/pi-ai";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createAgentSession } from "../src/core/sdk.js";
import { SessionManager } from "../src/core/session-manager.js";

describe("createAgentSession session manager defaults", () => {
	let tempDir: string;
	let cwd: string;
	let agentDir: string;

	beforeEach(() => {
		tempDir = join(tmpdir(), `pi-sdk-session-test-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		cwd = join(tempDir, "project");
		agentDir = join(tempDir, "agent");
		mkdirSync(cwd, { recursive: true });
		mkdirSync(agentDir, { recursive: true });
	});

	afterEach(() => {
		vi.unstubAllEnvs();
		if (tempDir && existsSync(tempDir)) {
			rmSync(tempDir, { recursive: true, force: true });
		}
	});

	it("uses agentDir for the default persisted session path", async () => {
		const model = getModel("anthropic", "claude-sonnet-4-5");
		expect(model).toBeTruthy();

		const { session } = await createAgentSession({
			cwd,
			agentDir,
			model: model!,
		});

		const expectedSessionDir = join(agentDir, "sessions");
		const sessionDir = session.sessionManager.getSessionDir();
		const sessionFile = session.sessionManager.getSessionFile();

		expect(sessionDir).toBe(expectedSessionDir);
		expect(sessionFile?.startsWith(`${expectedSessionDir}/`)).toBe(true);

		session.dispose();
	});

	it("keeps an explicit sessionManager override", async () => {
		const model = getModel("anthropic", "claude-sonnet-4-5");
		expect(model).toBeTruthy();

		const sessionManager = SessionManager.inMemory(cwd);
		const { session } = await createAgentSession({
			cwd,
			agentDir,
			model: model!,
			sessionManager,
		});

		expect(session.sessionManager).toBe(sessionManager);
		expect(session.sessionManager.isPersisted()).toBe(false);

		session.dispose();
	});

	it("derives cwd from an explicit sessionManager when cwd is omitted", async () => {
		const model = getModel("anthropic", "claude-sonnet-4-5");
		expect(model).toBeTruthy();

		const sessionCwd = join(tempDir, "session-project");
		mkdirSync(sessionCwd, { recursive: true });
		const sessionManager = SessionManager.inMemory(sessionCwd);
		const { session } = await createAgentSession({
			agentDir,
			model: model!,
			sessionManager,
			tools: ["ipython"],
		});

		expect(session.sessionManager).toBe(sessionManager);
		expect(session.systemPrompt).toContain(`Working directory: ${sessionCwd}`);

		const ipythonTool = session.agent.state.tools.find((tool) => tool.name === "ipython");
		expect(ipythonTool).toBeTruthy();
		const result = await ipythonTool!.execute("test", { code: "import os\nprint(os.getcwd())" });
		const output = result.content
			.filter((item): item is { type: "text"; text: string } => item.type === "text")
			.map((item) => item.text)
			.join("");

		expect(realpathSync(output.trim())).toBe(realpathSync(sessionCwd));

		session.dispose();
	}, 120_000);
	it("records primary transcript bytes once when an explicit persisted session is reopened", async () => {
		const model = getModel("anthropic", "claude-sonnet-4-5");
		expect(model).toBeTruthy();
		const metricsDir = join(tempDir, "metrics-reopen");
		const sessionPath = join(tempDir, "reopen.jsonl");
		const transcript = `${JSON.stringify({
			type: "session",
			version: 3,
			id: "sdk-reopen-session",
			timestamp: "2026-09-10T00:00:00.000Z",
			cwd,
		})}\n`;
		writeFileSync(sessionPath, transcript);
		const sessionManager = await SessionManager.openAsync(sessionPath);
		vi.stubEnv("PRIME_AGENT_PERFORMANCE_METRICS", "1");
		vi.stubEnv("PRIME_AGENT_PERFORMANCE_METRICS_DIR", metricsDir);

		const { session } = await createAgentSession({ cwd, agentDir, model: model!, sessionManager, tools: [] });
		const recorder = session.agent.performanceMetrics?.recorder as { logPath?: string } | undefined;
		expect(recorder?.logPath).toBeTruthy();
		await session.disposeAsync({ kernelSnapshot: false });

		const events = readFileSync(recorder!.logPath!, "utf8")
			.trim()
			.split("\n")
			.map((line) => JSON.parse(line) as Record<string, unknown>);
		const reopen = events.filter((event) => event.operation === "session_reopen");
		expect(reopen).toHaveLength(1);
		expect(reopen[0]).toMatchObject({
			identity: { component: "session" },
			outcome: "success",
			measurements: {
				read_bytes: Buffer.byteLength(transcript),
				total_ms: null,
				reopen_ms: null,
			},
		});
	});

	it("contains load-observation telemetry failures without blocking session creation", async () => {
		const model = getModel("anthropic", "claude-sonnet-4-5");
		expect(model).toBeTruthy();
		const sessionManager = SessionManager.inMemory(cwd);
		vi.spyOn(sessionManager, "getLoadObservation").mockImplementation(() => {
			throw new Error("synthetic disposable observation failure");
		});
		vi.stubEnv("PRIME_AGENT_PERFORMANCE_METRICS", "1");
		vi.stubEnv("PRIME_AGENT_PERFORMANCE_METRICS_DIR", join(tempDir, "metrics-observation-failure"));

		const { session } = await createAgentSession({ cwd, agentDir, model: model!, sessionManager, tools: [] });
		expect(session).toBeTruthy();
		await session.disposeAsync({ kernelSnapshot: false });
	});

	it("plumbs one opt-in recorder through the session and flushes it on async disposal", async () => {
		const model = getModel("anthropic", "claude-sonnet-4-5");
		expect(model).toBeTruthy();
		const metricsDir = join(tempDir, "metrics");
		vi.stubEnv("PRIME_AGENT_PERFORMANCE_METRICS", "1");
		vi.stubEnv("PRIME_AGENT_PERFORMANCE_METRICS_DIR", metricsDir);

		const { session } = await createAgentSession({
			cwd,
			agentDir,
			model: model!,
			tools: [],
		});
		const recorder = session.agent.performanceMetrics?.recorder;
		expect(recorder).toBeTruthy();
		recorder!.record({
			operation: "recorder",
			identity: { component: "recorder" },
			outcome: "success",
		});
		const logPath = (recorder as { logPath?: string }).logPath;
		expect(logPath).toBeTruthy();

		await session.disposeAsync({ kernelSnapshot: false });
		expect(readFileSync(logPath!, "utf8")).toContain('"operation":"recorder"');
	});
});
