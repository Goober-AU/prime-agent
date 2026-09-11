import { randomUUID } from "node:crypto";
import { existsSync, mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { lock, lockSync } from "proper-lockfile";
import { writeFileAtomicSync } from "../../utils/atomic-file.js";
import {
	applyRefinementProposal,
	type HarnessState,
	type RefinementProposal,
	type RefinementResult,
} from "../refinement/refinement.js";
import { hash, type MemorySource } from "./evidence.js";
import type { ProjectIdentity } from "./project.js";

export interface MemorySettings {
	recall: boolean;
	learning: boolean;
	maxRecallChars: number;
	maxRecallEntries: number;
	maxExtractionTokens: number;
	maxImportBytes: number;
	maxImportChunkChars: number;
	maxImportChunksPerRun: number;
	shared?: { url: string; tokenFile: string } | null;
}
export const DEFAULT_MEMORY_SETTINGS: MemorySettings = {
	recall: true,
	learning: true,
	maxRecallChars: 6000,
	maxRecallEntries: 6,
	maxExtractionTokens: 4096,
	maxImportBytes: 32 * 1024 * 1024,
	maxImportChunkChars: 40000,
	maxImportChunksPerRun: 4,
};
export interface MemoryDocument extends HarnessState {
	memory: {
		schema: 1;
		projectId: string;
		revision: number;
		history: RefinementResult[];
		events: Record<string, string>;
	};
}
export function emptyDocument(projectId: string): MemoryDocument {
	return {
		schema: 1,
		entries: { memory: {}, prompt: {}, skill: {}, subagent: {} },
		refinements: [],
		memory: { schema: 1, projectId, revision: 0, history: [], events: {} },
	};
}
export function record(value: unknown): Record<string, unknown> {
	if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Expected an object");
	return value as Record<string, unknown>;
}
export function validateDocument(value: unknown, projectId: string): MemoryDocument {
	const doc = record(value);
	const metadata = record(doc.memory);
	if (
		doc.schema !== 1 ||
		metadata.schema !== 1 ||
		metadata.projectId !== projectId ||
		!Number.isSafeInteger(metadata.revision) ||
		Number(metadata.revision) < 0 ||
		!Array.isArray(metadata.history) ||
		!Array.isArray(doc.refinements)
	)
		throw new Error("Invalid project memory snapshot");
	record(metadata.events);
	const entries = record(doc.entries);
	for (const kind of ["memory", "prompt", "skill", "subagent"]) {
		for (const [id, raw] of Object.entries(record(entries[kind]))) {
			const entry = record(raw);
			if (
				entry.id !== id ||
				entry.kind !== kind ||
				typeof entry.title !== "string" ||
				typeof entry.content !== "string" ||
				!Number.isSafeInteger(entry.version)
			)
				throw new Error("Invalid memory entry");
			if (record(entry.metadata).projectId !== projectId) throw new Error("Entry belongs to another project");
			record(entry.reference);
			record(entry.arguments);
		}
	}
	return value as MemoryDocument;
}
export function readJson(file: string): unknown {
	return JSON.parse(readFileSync(file, "utf8"));
}
export function writeJson(file: string, value: unknown): void {
	writeFileAtomicSync(file, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600, fsync: true, fsyncDir: true });
}
export function validateSettings(value: unknown): Partial<MemorySettings> {
	const source = record(value);
	const result: Partial<MemorySettings> = {};
	for (const key of ["recall", "learning"] as const) {
		if (source[key] !== undefined) {
			if (typeof source[key] !== "boolean") throw new Error(`${key} must be boolean`);
			result[key] = source[key];
		}
	}
	const limits = {
		maxRecallChars: [0, 32000],
		maxRecallEntries: [0, 50],
		maxExtractionTokens: [256, 32000],
		maxImportBytes: [1024, 128 * 1024 * 1024],
		maxImportChunkChars: [1000, 80000],
		maxImportChunksPerRun: [1, 64],
	};
	for (const key of Object.keys(limits) as (keyof typeof limits)[]) {
		const n = source[key];
		if (n !== undefined) {
			if (typeof n !== "number" || !Number.isSafeInteger(n) || n < limits[key][0] || n > limits[key][1])
				throw new Error(`Invalid ${key}`);
			result[key] = n;
		}
	}
	if (source.shared === null) result.shared = null;
	else if (source.shared !== undefined) {
		const shared = record(source.shared);
		if (typeof shared.url !== "string" || typeof shared.tokenFile !== "string" || !shared.tokenFile)
			throw new Error("Sharing requires url and tokenFile");
		const url = new URL(shared.url);
		if (
			url.username ||
			url.password ||
			url.search ||
			url.hash ||
			(url.protocol !== "https:" &&
				!(url.protocol === "http:" && ["127.0.0.1", "[::1]", "localhost"].includes(url.hostname)))
		)
			throw new Error("Use HTTPS or a loopback SSH tunnel for memory sharing");
		result.shared = { url: shared.url.replace(/\/$/, ""), tokenFile: shared.tokenFile };
	}
	return result;
}
export class MemoryStore {
	readonly dir: string;
	readonly path: string;
	readonly hostId: string;
	constructor(
		readonly agentDir: string,
		readonly project: ProjectIdentity,
	) {
		if (!/^project_[A-Za-z0-9_-]{1,80}$/.test(project.id)) throw new Error("Invalid project ID");
		this.dir = join(agentDir, "memory", "projects", project.id);
		this.path = join(this.dir, "harness_state.json");
		const root = join(agentDir, "memory");
		mkdirSync(root, { recursive: true, mode: 0o700 });
		const hostPath = join(root, "host-id.json");
		if (!existsSync(hostPath)) {
			const release = lockSync(root);
			try {
				if (!existsSync(hostPath)) writeJson(hostPath, { id: randomUUID() });
			} finally {
				release();
			}
		}
		const host = record(readJson(hostPath));
		if (typeof host.id !== "string" || !/^[a-f0-9-]{36}$/.test(host.id))
			throw new Error("Invalid memory host identity");
		this.hostId = host.id;
	}
	read(): MemoryDocument {
		return existsSync(this.path)
			? validateDocument(readJson(this.path), this.project.id)
			: emptyDocument(this.project.id);
	}
	settings(): MemorySettings {
		const globalFile = join(this.agentDir, "settings.json");
		const global = existsSync(globalFile) ? record(readJson(globalFile)) : {};
		const localFile = join(this.dir, "settings.json");
		return {
			...DEFAULT_MEMORY_SETTINGS,
			...validateSettings(global.memory ?? {}),
			...(existsSync(localFile) ? validateSettings(readJson(localFile)) : {}),
		};
	}
	async exclusive<T>(fn: () => T | Promise<T>): Promise<T> {
		mkdirSync(this.dir, { recursive: true, mode: 0o700 });
		const release = await lock(this.dir, { retries: { retries: 30, minTimeout: 20, maxTimeout: 200 }, stale: 30000 });
		try {
			return await fn();
		} finally {
			await release();
		}
	}
	async configure(value: unknown): Promise<MemorySettings> {
		const patch = validateSettings(value);
		return this.exclusive(() => {
			const path = join(this.dir, "settings.json");
			writeJson(path, { ...(existsSync(path) ? record(readJson(path)) : {}), ...patch });
			return this.settings();
		});
	}
	backup(doc = this.read()): string {
		const body = JSON.stringify(doc);
		const id = `backup_${doc.memory.revision}_${hash(body).slice(0, 16)}`;
		const dir = join(this.dir, "backups");
		mkdirSync(dir, { recursive: true, mode: 0o700 });
		writeJson(join(dir, `${id}.json`), { schema: 1, sha256: hash(body), state: doc });
		return id;
	}
	async restore(id: string): Promise<number> {
		if (!/^backup_\d+_[a-f0-9]{16}$/.test(id)) throw new Error("Invalid backup ID");
		return this.exclusive(() => {
			const staged = record(readJson(join(this.dir, "backups", `${id}.json`)));
			if (staged.schema !== 1 || hash(JSON.stringify(staged.state)) !== staged.sha256)
				throw new Error("Backup checksum mismatch");
			const restored = validateDocument(staged.state, this.project.id);
			const current = this.read();
			this.backup(current);
			restored.memory.revision = current.memory.revision + 1;
			// Preserve delivery receipts so restoring old content cannot replay already committed jobs.
			restored.memory.events = { ...restored.memory.events, ...current.memory.events };
			writeJson(this.path, restored);
			return restored.memory.revision;
		});
	}
	async apply(
		proposal: RefinementProposal,
		options: {
			eventId: string;
			expectedRevision: number;
			sources?: MemorySource[];
			host?: boolean;
			automatic?: boolean;
			replaceMetadata?: boolean;
		},
	): Promise<RefinementResult> {
		return this.exclusive(() => {
			const doc = this.read();
			const fingerprint = hash(
				JSON.stringify({
					proposal,
					sources: options.sources ?? [],
					host: options.host ?? false,
					replaceMetadata: options.replaceMetadata ?? false,
				}),
			);
			const prior = doc.memory.events[options.eventId];
			if (prior) {
				if (prior !== fingerprint) throw new Error("Event ID reused with different content");
				const result = doc.memory.history.find((item) => item.id === options.eventId);
				if (!result)
					throw new Error("Event already committed before restore; inspect current memory before resubmitting");
				return result;
			}
			if (options.automatic && !this.settings().learning) throw new Error("Automatic learning is paused");
			if (doc.memory.revision !== options.expectedRevision)
				throw new Error("Memory changed; review current revision before applying");
			if (!/^[A-Za-z0-9_-]{1,160}$/.test(options.eventId)) throw new Error("Invalid event ID");
			const edits = proposal.edits.map((edit) => {
				if (
					edit.id &&
					(!/^[A-Za-z0-9_-]{1,160}$/.test(edit.id) || ["__proto__", "constructor", "prototype"].includes(edit.id))
				)
					throw new Error("Invalid memory ID");
				if (edit.metadata?.projectId && edit.metadata.projectId !== this.project.id)
					throw new Error("Memory belongs to another project");
				const before = edit.id ? doc.entries[edit.kind]?.[edit.id] : undefined;
				if (before?.metadata.hostId && before.metadata.hostId !== this.hostId)
					throw new Error("Memory belongs to another host");
				const metadata = { ...(options.replaceMetadata ? {} : before?.metadata), ...edit.metadata };
				if (metadata.hostId && metadata.hostId !== this.hostId) throw new Error("Memory belongs to another host");
				const hostId = options.host ? this.hostId : metadata.hostId;
				if (edit.action === "create") {
					const duplicate = Object.values(doc.entries[edit.kind] ?? {}).find(
						(entry) =>
							entry.content.trim() === edit.content?.trim() &&
							entry.metadata.hostId === hostId &&
							!entry.metadata.supersededBy,
					);
					if (duplicate) throw new Error(`Exact duplicate: ${duplicate.id}`);
				}
				return {
					...edit,
					metadata: {
						...metadata,
						projectId: this.project.id,
						hostId,
						sources: options.sources ?? metadata.sources ?? [],
						status: metadata.supersededBy ? "superseded" : "current",
					},
				};
			});
			const candidate = structuredClone(doc);
			const result = applyRefinementProposal(
				candidate,
				{ ...proposal, edits },
				{ id: options.eventId, scope: "local" },
			);
			if (result.appliedEdits.some((edit) => !edit.applied))
				throw new Error(
					result.appliedEdits
						.filter((edit) => !edit.applied)
						.map((edit) => edit.error)
						.join("; "),
				);
			result.harnessStatePath = this.path;
			candidate.memory.revision++;
			candidate.memory.events[options.eventId] = fingerprint;
			candidate.memory.history.push(result);
			this.backup(doc);
			writeJson(this.path, candidate);
			return result;
		});
	}
	async rollback(id: string, expectedRevision: number): Promise<RefinementResult> {
		const doc = this.read();
		const result = doc.memory.history.find((item) => item.id === id);
		if (!result) throw new Error("Unknown refinement ID");
		const edits = [...result.appliedEdits]
			.reverse()
			.filter((edit) => edit.applied)
			.map((edit) => {
				if (JSON.stringify(doc.entries[edit.kind][edit.id]) !== JSON.stringify(edit.after))
					throw new Error(`Entry ${edit.id} has changed since this edit`);
				return edit.before
					? { ...edit.before, action: edit.after ? ("update" as const) : ("create" as const) }
					: { action: "delete" as const, kind: edit.kind, id: edit.id };
			});
		return this.apply(
			{ summary: `Rollback ${id}`, rationale: "Explicit rollback", expectedOutcome: "Restore prior memory", edits },
			{ eventId: `rollback_${id}_${expectedRevision}`, expectedRevision, replaceMetadata: true },
		);
	}
}
