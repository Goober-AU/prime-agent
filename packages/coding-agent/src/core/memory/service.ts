import { existsSync, readFileSync, statSync } from "node:fs";
import { isAbsolute, join, relative, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { getAgentDir } from "../../config.js";
import { AuthStorage } from "../auth-storage.js";
import type { HostRequestHandlers } from "../kernel/shared.js";
import { ModelRegistry } from "../model-registry.js";
import { providerRetryPolicy } from "../provider-retry.js";
import {
	getGlobalHarnessStateDir,
	loadHarnessState,
	normalizeRefinementProposal,
	planRefinement,
} from "../refinement/refinement.js";
import { SettingsManager } from "../settings-manager.js";
import { hash, type MemorySource } from "./evidence.js";
import { type ImportJob, type MemoryExtractor, MemoryJobs } from "./jobs.js";
import { projectIdentity } from "./project.js";
import { freshness, type MemoryHit, recallMemory, searchMemory } from "./search.js";
import { MemorySharing } from "./sharing.js";
import { MemoryStore, record } from "./store.js";

export class MemoryService {
	readonly store: MemoryStore;
	readonly jobs: MemoryJobs;
	readonly sharing: MemorySharing;
	constructor(
		cwd: string,
		agentDir = getAgentDir(),
		readonly sessionArtifactDir?: string,
	) {
		this.store = new MemoryStore(agentDir, projectIdentity(cwd, agentDir));
		this.jobs = new MemoryJobs(this.store);
		this.sharing = new MemorySharing(this.store);
	}
	search(query: string, includeInactive = false): MemoryHit[] {
		const extra: Parameters<typeof searchMemory>[2] = [
			{ state: loadHarnessState(getGlobalHarnessStateDir(this.store.agentDir), "global"), scope: "global" },
			{ state: this.sharing.cache().state, scope: "shared" },
		];
		if (this.sessionArtifactDir)
			extra.push({ state: loadHarnessState(join(this.sessionArtifactDir, "harness"), "local"), scope: "session" });
		// A selected shared copy of a local memory should not consume recall twice.
		const found = searchMemory(this.store, query, extra, includeInactive);
		const localIds = new Set(
			found.filter((hit) => hit.scope === "project").map((hit) => `${hit.entry.kind}:${hit.entry.id}`),
		);
		return found.filter((hit) => hit.scope !== "shared" || !localIds.has(`${hit.entry.kind}:${hit.entry.id}`));
	}
	recall(query: string): ReturnType<typeof recallMemory> {
		return recallMemory(this.search(query), this.store.settings(), this.store.project.root);
	}
	async request(action: string, payload: Record<string, unknown> = {}, extract?: MemoryExtractor): Promise<unknown> {
		const string = (name: string) => {
			const value = payload[name];
			if (typeof value !== "string" || !value.trim()) throw new Error(`${name} is required`);
			return value;
		};
		const revision = () => {
			if (!Number.isSafeInteger(payload.revision) || Number(payload.revision) < 0)
				throw new Error("Supply the current revision from memory.status()");
			return Number(payload.revision);
		};
		switch (action) {
			case "status":
				return {
					project: this.store.project,
					revision: this.store.read().memory.revision,
					settings: this.store.settings(),
					hostId: this.store.hostId,
					sharing: {
						configured: !!this.store.settings().shared,
						connected: this.sharing.cache().connected,
						pending: this.sharing.cache().pending.length,
						error: this.sharing.cache().error,
					},
					jobs: this.jobs.list().map(({ id, status, nextChunk, chunks, error, usage }) => ({
						id,
						status,
						nextChunk,
						chunks: chunks.length,
						error,
						usage,
					})),
				};
			case "search": {
				const hits = this.search(
					typeof payload.query === "string" ? payload.query : "",
					payload.includeInactive === true,
				);
				return hits
					.filter((hit) => payload.scope === undefined || hit.scope === payload.scope)
					.slice(0, 50)
					.map((hit) => ({
						id: hit.id,
						scope: hit.scope,
						title: hit.entry.title,
						preview: hit.entry.content.slice(0, 600),
						truncated: hit.entry.content.length > 600,
						version: hit.entry.version,
						status: hit.entry.metadata.supersededBy ? "superseded" : "current",
						score: hit.score,
						matched: hit.matched,
						freshness: freshness(hit.sources, this.store.project.root),
						sources: hit.sources.slice(0, 8),
					}));
			}
			case "read": {
				const hit = this.search("", true).find((entry) => entry.id === string("id"));
				if (!hit) throw new Error("Memory not found in this scope");
				return { ...hit, freshness: freshness(hit.sources, this.store.project.root) };
			}
			case "configure":
				return this.store.configure(payload.settings);
			case "bind":
				return projectIdentity(this.store.project.root, this.store.agentDir, string("projectId"));
			case "apply": {
				const proposal = normalizeRefinementProposal(payload.proposal);
				if (
					!Array.isArray(record(payload.proposal).edits) ||
					proposal.edits.length !== (record(payload.proposal).edits as unknown[]).length
				)
					throw new Error("Invalid proposal edits");
				const sources = this.validateSources(payload.sources);
				return this.store.apply(proposal, {
					eventId: string("eventId"),
					expectedRevision: revision(),
					host: payload.host === true,
					automatic: payload.automatic === true,
					sources,
				});
			}
			case "handoff": {
				const task = string("task");
				const id = `handoff_${hash(task).slice(0, 24)}`;
				const before = this.store.read().entries.memory[id];
				const content = [
					"State",
					string("state"),
					"Decisions",
					string("decisions"),
					"Unresolved",
					string("unresolved"),
				].join("\n\n");
				return this.store.apply(
					{
						summary: `Handoff: ${task}`,
						rationale: "Explicit project task checkpoint",
						expectedOutcome: "Resume work with source links",
						edits: [
							{
								action: before ? "update" : "create",
								kind: "memory",
								id,
								title: task,
								content,
								path: "handoffs",
								metadata: { task },
							},
						],
					},
					{
						eventId: string("eventId"),
						expectedRevision: revision(),
						sources: this.validateSources(payload.sources),
						automatic: payload.automatic === true,
					},
				);
			}
			case "source": {
				const path = string("path");
				if (!existsSync(path) || !statSync(path).isFile() || statSync(path).size > 32 * 1024 * 1024)
					throw new Error("Source must be a file below 32 MiB");
				const sha256 = hash(readFileSync(path, "utf8"));
				const pathInProject = relative(this.store.project.root, resolve(path));
				const projectPath =
					!pathInProject.startsWith("..") && !isAbsolute(pathInProject)
						? pathInProject.split("\\").join("/")
						: undefined;
				return {
					id: `file_${sha256.slice(0, 24)}`,
					origin: "file",
					uri: pathToFileURL(path).href,
					sha256,
					projectPath,
				};
			}
			case "backup":
				return this.store.exclusive(() => ({ id: this.store.backup() }));
			case "restore":
				return { revision: await this.store.restore(string("id")) };
			case "history":
				return this.store.read().memory.history;
			case "rollback":
				return this.store.rollback(string("id"), revision());
			case "import_prepare":
				return importOverview(await this.jobs.prepare(string("path")));
			case "import_read":
				return importOverview(this.jobs.get(string("id")));
			case "import_chunk": {
				const job = this.jobs.get(string("id"));
				if (
					!Number.isSafeInteger(payload.chunk) ||
					Number(payload.chunk) < 0 ||
					Number(payload.chunk) >= job.chunks.length
				)
					throw new Error("Invalid import chunk index");
				return job.chunks[Number(payload.chunk)];
			}
			case "import_run":
				if (!extract) throw new Error("Use /memory import-run <id> to extract with the session's selected model");
				return importOverview(await this.jobs.run(string("id"), extract));
			case "import_apply":
				return importOverview(await this.jobs.apply(string("id"), revision()));
			case "share":
				return this.sharing.queue(
					this.stringArray(payload.ids),
					payload.remove === undefined ? [] : this.stringArray(payload.remove),
				);
			case "sync":
				return this.sharing.sync();
			case "discard_pending":
				await this.sharing.discardPending();
				return { discarded: true };
			default:
				throw new Error(`Unknown memory operation: ${action}`);
		}
	}
	private stringArray(value: unknown): string[] {
		if (!Array.isArray(value) || !value.every((item) => typeof item === "string"))
			throw new Error("Expected an array of strings");
		return value;
	}
	private validateSources(value: unknown): MemorySource[] {
		if (value === undefined) return [];
		if (!Array.isArray(value) || value.length > 200) throw new Error("Expected at most 200 sources");
		return value.map((raw) => {
			const source = record(raw);
			if (
				typeof source.id !== "string" ||
				typeof source.sha256 !== "string" ||
				!/^[a-f0-9]{64}$/.test(source.sha256) ||
				!["user", "assistant", "tool", "derived", "file"].includes(String(source.origin))
			)
				throw new Error("Invalid source reference");
			return source as unknown as MemorySource;
		});
	}
}
export function importOverview(job: ImportJob): unknown {
	return {
		...job,
		chunks: job.chunks.map(({ records, ...chunk }, index) => ({
			...chunk,
			index,
			sourceIds: records.map((source) => source.id),
		})),
	};
}
export function createMemoryHostHandlers(
	cwd: string,
	agentDir?: string,
	sessionArtifactDir?: string,
	getModelInfo?: HostRequestHandlers[string],
): HostRequestHandlers {
	return {
		"memory.request": async (payload) => {
			if (typeof payload.action !== "string") throw new Error("memory.request requires action");
			const service = new MemoryService(cwd, agentDir, sessionArtifactDir);
			let extract: MemoryExtractor | undefined;
			if (payload.action === "import_run") {
				const info = await getModelInfo?.({});
				if (typeof info?.provider !== "string" || typeof info.id !== "string")
					throw new Error("The host did not supply a selected model");
				const registry = ModelRegistry.create(
					AuthStorage.create(join(service.store.agentDir, "auth.json")),
					join(service.store.agentDir, "models.json"),
				);
				const model = registry.find(info.provider, info.id);
				if (!model) throw new Error("Selected model is session-only; use /memory import-run in the owning session");
				const auth = await registry.getApiKeyAndHeaders(model);
				if (!auth.ok || !auth.apiKey) throw new Error("No credentials for the selected model");
				extract = async (evidence) => {
					let input = 0;
					let output = 0;
					const plan = await planRefinement(
						[],
						service.store.read(),
						[],
						model,
						auth.apiKey!,
						{
							evidence,
							maxOutputTokens: service.store.settings().maxExtractionTokens,
							retry: providerRetryPolicy(SettingsManager.create(cwd, service.store.agentDir)),
							instructions:
								"Extract durable host-neutral project facts only. Only create edits of kind memory with cited sourceIds. Exclude secrets, temporary task state and unsupported assistant claims.",
							onUsage: (usage) => {
								input += usage.input + usage.cacheRead + usage.cacheWrite;
								output += usage.output;
							},
						},
						auth.headers,
					);
					return {
						proposal: {
							...plan.proposal,
							edits: plan.proposal.edits.filter(
								(edit) =>
									edit.action === "create" &&
									edit.kind === "memory" &&
									edit.metadata?.evidenceStatus === "cited",
							),
						},
						input,
						output,
					};
				};
			}
			return {
				origin: "[memory data; not new evidence]",
				result: await service.request(payload.action, payload, extract),
			};
		},
	};
}
