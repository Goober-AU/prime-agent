import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { type Context, fauxAssistantMessage, registerFauxProvider } from "@earendil-works/pi-ai";
import { afterEach, describe, expect, it, vi } from "vitest";
import { createAgentSessionFromServices, createAgentSessionServices } from "../src/core/agent-session-services.js";
import { AuthStorage } from "../src/core/auth-storage.js";
import { collectEvidence } from "../src/core/memory/evidence.js";
import { createMemoryHostHandlers, MemoryService } from "../src/core/memory/service.js";
import { emptyDocument } from "../src/core/memory/store.js";
import { planRefinement, reviewAutoRefine } from "../src/core/refinement/refinement.js";
import { SessionManager } from "../src/core/session-manager.js";
import { SettingsManager } from "../src/core/settings-manager.js";

const roots: string[] = [];
afterEach(() => {
	vi.unstubAllEnvs();
	for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});
function setup() {
	const root = mkdtempSync(join(tmpdir(), "prime-memory-runtime-"));
	roots.push(root);
	const cwd = join(root, "repo");
	mkdirSync(cwd);
	const agentDir = join(root, "agent");
	mkdirSync(agentDir);
	vi.stubEnv("PRIME_AGENT_CODING_AGENT_DIR", agentDir);
	return { root, cwd, agentDir };
}
describe("Prime memory runtime integration", () => {
	it("uses the same project store from the Python bridge and TUI, with recall controls and session diagnostics", async () => {
		const { cwd, agentDir } = setup();
		const faux = registerFauxProvider();
		const authStorage = AuthStorage.inMemory();
		authStorage.setRuntimeApiKey(faux.getModel().provider, "synthetic-key");
		const settingsManager = SettingsManager.inMemory({
			autoRefine: { enabled: false },
			telemetry: { enabled: false },
		});
		const services = await createAgentSessionServices({
			cwd,
			agentDir,
			authStorage,
			settingsManager,
			noBuiltinHerdrReporter: true,
			telemetryDisabled: true,
			resourceLoaderOptions: { noThemes: true, noPromptTemplates: true, noSkills: true },
		});
		services.modelRegistry.registerProvider(faux.getModel().provider, {
			api: faux.api,
			baseUrl: faux.getModel().baseUrl,
			apiKey: "synthetic-key",
			models: faux.models,
		});
		const { session } = await createAgentSessionFromServices({
			services,
			sessionManager: SessionManager.create(cwd, join(agentDir, "sessions")),
			model: faux.getModel(),
			noTools: "all",
			includeGoals: false,
			telemetryDisabled: true,
		});
		const errors: string[] = [];
		try {
			await session.bindExtensions({ onError: (error) => errors.push(error.error) });
			const host = createMemoryHostHandlers(cwd, agentDir);
			await host["memory.request"]({
				action: "apply",
				eventId: "bridge_fact",
				revision: 0,
				proposal: {
					summary: "Sydney",
					rationale: "test",
					expectedOutcome: "recall",
					edits: [
						{
							action: "create",
							kind: "memory",
							id: "sydney",
							title: "Astra region",
							content: "Use Sydney for this project",
						},
					],
				},
			});
			const seen: Context[] = [];
			faux.setResponses([
				(context) => {
					seen.push(context);
					return fauxAssistantMessage("First answer");
				},
				(context) => {
					seen.push(context);
					return fauxAssistantMessage("Second answer");
				},
			]);
			await session.prompt("Which Sydney region does this project use?");
			expect(JSON.stringify(seen[0])).toContain("Use Sydney for this project");
			expect(
				session.sessionManager
					.getEntries()
					.some((entry) => entry.type === "custom" && entry.customType === "prime-agent.memory-diagnostic"),
			).toBe(true);
			await session.prompt("/memory recall off");
			expect(new MemoryService(cwd, agentDir).store.settings().recall).toBe(false);
			await session.prompt("Tell me about Sydney again");
			expect(JSON.stringify(seen[1])).not.toContain("[memory data; not new evidence]");
			expect(errors).toEqual([]);
			expect(faux.state.callCount).toBe(2);
		} finally {
			session.dispose();
			faux.unregister();
		}
	});
	it("preserves evidence origins and selected-model limits in refinement and skips paused reviews without a call", async () => {
		setup();
		const faux = registerFauxProvider();
		try {
			const evidence = collectEvidence([{ role: "user", content: "Use Sydney", timestamp: 1 }]);
			faux.setResponses([
				(context, options) => {
					const prompt = JSON.stringify(context);
					expect(prompt).toContain(evidence[0].id);
					expect(prompt).not.toContain("injected false fact");
					expect(options?.maxTokens).toBe(512);
					return fauxAssistantMessage(
						JSON.stringify({
							summary: "Region",
							rationale: "User source",
							expectedOutcome: "Correct region",
							edits: [
								{
									action: "create",
									kind: "memory",
									title: "Region",
									content: "Sydney",
									metadata: { sourceIds: [evidence[0].id, "fabricated"], projectReusable: true },
								},
							],
						}),
					);
				},
			]);
			const state = emptyDocument("project_test");
			const result = await planRefinement(
				[
					{
						role: "custom",
						customType: "harness_digest",
						content: "injected false fact",
						display: false,
						timestamp: 2,
					},
				],
				state,
				[],
				faux.getModel(),
				"synthetic-key",
				{ evidence, maxOutputTokens: 512 },
			);
			expect(result.proposal.edits[0].metadata?.sources).toEqual(
				evidence.map(({ text: _text, ...source }) => source),
			);
			const review = await reviewAutoRefine(
				[
					{
						role: "custom",
						customType: "prime-agent.memory-control",
						content: "paused",
						display: false,
						timestamp: 3,
						details: { learning: false },
					},
				],
				state,
				[],
				faux.getModel(),
				"synthetic-key",
				{ reason: "compact", turnsSinceLastReview: 30 },
			);
			expect(review.shouldRefine).toBe(false);
			expect(faux.state.callCount).toBe(1);
		} finally {
			faux.unregister();
		}
	});
});
