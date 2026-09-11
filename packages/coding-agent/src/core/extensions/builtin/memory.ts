import { pathToFileURL } from "node:url";
import type { AgentMessage } from "@earendil-works/pi-agent-core";
import { collectEvidence, type Evidence, hash, MEMORY_RECALL_TYPE, messageEvidence } from "../../memory/evidence.js";
import type { MemoryExtractor } from "../../memory/jobs.js";
import { MemoryService } from "../../memory/service.js";
import { withoutHarnessDigestsForCompaction } from "../../messages.js";
import { providerRetryPolicy } from "../../provider-retry.js";
import {
	getLocalHarnessStateDir,
	loadHarnessState,
	planRefinement,
	type RefinementProposal,
} from "../../refinement/refinement.js";
import { getSessionArtifactPath } from "../../session-manager.js";
import type { SettingsManager } from "../../settings-manager.js";
import type { ExtensionContext, ExtensionFactory } from "../types.js";

export function createMemoryExtension(agentDir: string, settingsManager: SettingsManager): ExtensionFactory {
	return (pi) => {
		const diagnostic = (data: unknown) => {
			try {
				pi.appendEntry("prime-agent.memory-diagnostic", data);
			} catch {
				/* Diagnostics cannot block a turn. */
			}
		};
		// Bind per session: a shared resource loader must not leak a parent's recall into children.
		const services = new Map<string, MemoryService>();
		const recalledTurns = new Map<string, { key: string; recall: ReturnType<MemoryService["recall"]> }>();
		const sessionKey = (ctx: ExtensionContext) => `${ctx.cwd}\0${ctx.sessionManager.getSessionId()}`;
		const service = (ctx: ExtensionContext) => {
			const key = sessionKey(ctx);
			let memory = services.get(key);
			if (!memory) {
				memory = new MemoryService(
					ctx.cwd,
					agentDir,
					ctx.sessionManager.getSessionFile()
						? getSessionArtifactPath(ctx.sessionManager.getSessionDir(), ctx.sessionManager.getSessionId())
						: undefined,
				);
				services.set(key, memory);
			}
			return memory;
		};
		pi.on("session_shutdown", (_event, ctx) => {
			services.delete(sessionKey(ctx));
			recalledTurns.delete(sessionKey(ctx));
		});
		const evidence = (ctx: ExtensionContext): Evidence[] =>
			ctx.sessionManager.getBranch().flatMap((entry) => {
				if (entry.type !== "message") return [];
				const file = ctx.sessionManager.getSessionFile();
				const source = messageEvidence(
					entry.message,
					file ? `${pathToFileURL(file).href}#${entry.id}` : undefined,
					entry.id,
				);
				return source ? [source] : [];
			});
		const extractor =
			(ctx: ExtensionContext, memory: MemoryService): MemoryExtractor =>
			async (records) => {
				const model = ctx.model;
				if (!model) throw new Error("Select a model before extracting memory");
				const auth = await ctx.modelRegistry.getApiKeyAndHeaders(model);
				if (!auth.ok || !auth.apiKey) throw new Error("No credentials for the selected model");
				let input = 0;
				let output = 0;
				const plan = await planRefinement(
					[],
					memory.store.read(),
					[],
					model,
					auth.apiKey,
					{
						evidence: records,
						maxOutputTokens: memory.store.settings().maxExtractionTokens,
						retry: providerRetryPolicy(settingsManager),
						instructions:
							"Extract only durable project memory. Return create edits of kind memory with sourceIds. Do not update or delete existing entries. Exclude host facts, credentials, unsupported assistant assertions and transient task state. Raw sources remain available, so do not copy entire logs into memory.",
						onUsage: (usage) => {
							input += usage.input + usage.cacheRead + usage.cacheWrite;
							output += usage.output;
						},
					},
					auth.headers,
					ctx.signal,
				);
				const edits = plan.proposal.edits.filter(
					(edit) =>
						edit.action === "create" && edit.kind === "memory" && edit.metadata?.evidenceStatus === "cited",
				);
				diagnostic({
					operation: "extract",
					input,
					output,
					records: records.length,
					proposed: edits.length,
				});
				return { proposal: { ...plan.proposal, edits }, input, output };
			};
		pi.registerCommand("memory", {
			description: "Inspect project memory, recall, learning, imports and optional sharing",
			getArgumentCompletions: (prefix) =>
				[
					"status",
					"search",
					"read",
					"recall",
					"learning",
					"history",
					"backup",
					"restore",
					"rollback",
					"import-prepare",
					"import-run",
					"import-read",
					"import-apply",
					"share",
					"sync",
					"bind",
					"configure",
				]
					.filter((value) => value.startsWith(prefix))
					.map((value) => ({ value, label: value })),
			handler: async (args, ctx) => {
				const memory = service(ctx);
				const match = args.trim().match(/^(\S+)(?:\s+([\s\S]*))?$/);
				const action = (match?.[1] ?? "status").replaceAll("-", "_");
				const rest = match?.[2] ?? "";
				let result: unknown;
				if (action === "recall" || action === "learning") {
					if (!["on", "off"].includes(rest)) throw new Error(`Usage: /memory ${action} on|off`);
					result = await memory.store.configure({ [action]: rest === "on" });
				} else {
					let payload: Record<string, unknown> = {};
					if (["search"].includes(action)) payload = { query: rest };
					else if (["read", "restore", "import_run", "import_read"].includes(action)) payload = { id: rest };
					else if (action === "import_prepare") payload = { path: rest };
					else if (action === "bind") payload = { projectId: rest };
					else if (action === "share") payload = { ids: rest.split(/\s+/).filter(Boolean) };
					else if (action === "configure") payload = { settings: JSON.parse(rest) };
					else if (["rollback", "import_apply"].includes(action)) {
						const [id, rev] = rest.split(/\s+/);
						payload = { id, revision: Number(rev) };
					} else if (rest) payload = JSON.parse(rest);
					result = await memory.request(action, payload, extractor(ctx, memory));
					if (action === "status")
						result = {
							...(result as Record<string, unknown>),
							autoRefine: settingsManager.getAutoRefineSettings(),
						};
				}
				if (action === "bind") services.delete(sessionKey(ctx));
				recalledTurns.delete(sessionKey(ctx));
				const current = service(ctx);
				const learning = current.store.settings().learning && settingsManager.getAutoRefineSettings().enabled;
				pi.sendMessage({
					customType: "prime-agent.memory-control",
					content: `Memory automatic learning: ${learning ? "enabled" : "paused"}.`,
					display: false,
					details: { learning, projectId: current.store.project.id },
				});
				const content = JSON.stringify(result, null, 2);
				pi.appendEntry("prime-agent.memory-command", { action, result });
				ctx.ui.notify(content, "info");
				// Existing custom-message transport also exposes command results to non-TUI clients.
				pi.sendMessage({
					customType: "prime-agent.memory-result",
					content,
					display: true,
					details: { action, result },
				});
			},
		});
		pi.on("before_agent_start", (_event, ctx) => {
			const memory = service(ctx);
			const learning = memory.store.settings().learning && settingsManager.getAutoRefineSettings().enabled;
			return {
				message: {
					customType: "prime-agent.memory-control",
					content: `Memory automatic learning: ${learning ? "enabled" : "paused"}.`,
					display: false,
					details: { learning, projectId: memory.store.project.id },
				},
			};
		});
		pi.on("context", (event, ctx) => {
			const started = performance.now();
			let messages = event.messages.filter(
				(message) => message.role !== "custom" || message.customType !== MEMORY_RECALL_TYPE,
			);
			try {
				const memory = service(ctx);
				if (!memory.store.settings().recall) return { messages: withoutHarnessDigestsForCompaction(messages) };
				const user = [...messages].reverse().find((message) => message.role === "user");
				const query = user ? (collectEvidence([user])[0]?.text ?? "") : "";
				const key = hash(`${user?.timestamp}:${query}:${JSON.stringify(memory.store.settings())}`);
				const cached = recalledTurns.get(sessionKey(ctx));
				const recalled =
					cached?.key === key ? cached.recall : query ? memory.recall(query) : { text: "", ids: [], chars: 0 };
				recalledTurns.set(sessionKey(ctx), { key, recall: recalled });
				if (recalled.text) {
					const note: AgentMessage = {
						role: "custom",
						customType: MEMORY_RECALL_TYPE,
						content: recalled.text,
						display: false,
						details: { ids: recalled.ids },
						timestamp: user?.timestamp ?? 0,
					};
					// Anchor recall before the current user turn, preserving it across subsequent tool requests.
					const index = user ? messages.lastIndexOf(user) : messages.length;
					messages = [...messages.slice(0, index), note, ...messages.slice(index)];
				}
				diagnostic({
					operation: "recall",
					projectId: memory.store.project.id,
					ids: recalled.ids,
					chars: recalled.chars,
					latencyMs: performance.now() - started,
				});
				return { messages };
			} catch {
				diagnostic({
					operation: "recall",
					status: "failed",
					latencyMs: performance.now() - started,
				});
				return { messages };
			}
		});
		pi.on("session_before_refine", async (event, ctx) => {
			const memory = service(ctx);
			if (event.preparation.trigger === "auto" && !memory.store.settings().learning) return { skip: true };
			const model = ctx.model;
			if (!model) return;
			const auth = await ctx.modelRegistry.getApiKeyAndHeaders(model);
			if (!auth.ok || !auth.apiKey) return;
			const plan = await planRefinement(
				[],
				event.preparation.planningState,
				event.preparation.history,
				model,
				auth.apiKey,
				{
					evidence: evidence(ctx),
					instructions: event.preparation.instructions,
					global: event.preparation.scope === "global",
					retry: providerRetryPolicy(settingsManager),
					maxOutputTokens: memory.store.settings().maxExtractionTokens,
					onUsage: (usage) => diagnostic({ operation: "refine", usage }),
				},
				auth.headers,
				event.signal,
			);
			if (event.preparation.trigger === "auto" && !memory.store.settings().learning) return { skip: true };
			return { proposal: plan.proposal };
		});
		pi.on("refine_complete", async (event, ctx) => {
			if (event.scope !== "local") return;
			const memory = service(ctx);
			if (!memory.store.settings().learning) return;
			const localDir = getLocalHarnessStateDir(memory.sessionArtifactDir);
			if (!localDir) return;
			const local = loadHarnessState(localDir, "local");
			const doc = memory.store.read();
			const changed = local.refinements.find((item) => item.id === event.id)?.changes ?? [];
			const edits: RefinementProposal["edits"] = [];
			for (const entry of Object.values(local.entries.memory)) {
				if (
					!changed.some(
						(change) => change === `create memory:${entry.id}` || change === `update memory:${entry.id}`,
					)
				)
					continue;
				if (entry.metadata.projectReusable !== true || entry.metadata.evidenceStatus !== "cited") continue;
				if (
					doc.entries.memory[entry.id] ||
					Object.values(doc.entries.memory).some((saved) => saved.content.trim() === entry.content.trim())
				)
					continue;
				edits.push({
					action: "create",
					kind: "memory",
					id: entry.id,
					title: entry.title,
					content: entry.content,
					path: entry.path,
					metadata: entry.metadata,
				});
			}
			if (!edits.length) return;
			try {
				await memory.store.apply(
					{
						summary: event.summary,
						rationale: `Project facts from ${event.id}`,
						expectedOutcome: "Retain evidence-backed project facts across sessions",
						edits,
					},
					{ eventId: `project_${event.id}`, expectedRevision: doc.memory.revision, automatic: true },
				);
			} catch {
				diagnostic({
					operation: "promote",
					status: "conflict",
					refinementId: event.id,
				});
			}
		});
	};
}
