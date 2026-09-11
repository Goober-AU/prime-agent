import { existsSync, mkdirSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import type { AgentMessage } from "@earendil-works/pi-agent-core";
import { lock } from "proper-lockfile";
import { writeFileAtomicSync } from "../../utils/atomic-file.js";
import type { RefinementProposal } from "../refinement/refinement.js";
import { type Evidence, hash, messageEvidence, serializeEvidence } from "./evidence.js";
import { type MemoryStore, readJson, record, writeJson } from "./store.js";

export interface ImportChunk {
	records: Evidence[];
	startLine: number;
	endLine: number;
	proposal?: RefinementProposal;
}
export interface ImportJob {
	schema: 1;
	id: string;
	source: string;
	sourceHash: string;
	status: "pending" | "running" | "preview" | "applied" | "failed";
	chunks: ImportChunk[];
	nextChunk: number;
	expectedRevision: number;
	error?: string;
	coverage: { lines: number; evidenceRecords: number; excludedLines: number[] };
	usage: { input: number; output: number };
	acceptedProposal?: RefinementProposal;
}
export type MemoryExtractor = (
	records: Evidence[],
) => Promise<{ proposal: RefinementProposal; input: number; output: number }>;
export class MemoryJobs {
	readonly dir: string;
	constructor(readonly store: MemoryStore) {
		this.dir = join(store.dir, "jobs");
	}
	private path(id: string): string {
		if (!/^import_[a-f0-9]{24}$/.test(id)) throw new Error("Invalid import ID");
		return join(this.dir, `${id}.json`);
	}
	get(id: string): ImportJob {
		return readJson(this.path(id)) as ImportJob;
	}
	list(): ImportJob[] {
		return existsSync(this.dir)
			? readdirSync(this.dir)
					.filter((name) => /^import_[a-f0-9]{24}\.json$/.test(name))
					.map((name) => this.get(name.slice(0, -5)))
			: [];
	}
	async prepare(source: string): Promise<ImportJob> {
		const settings = this.store.settings();
		if (!statSync(source).isFile() || statSync(source).size > settings.maxImportBytes)
			throw new Error("Selected session exceeds import size limit or is not a file");
		const raw = readFileSync(source, "utf8");
		const sourceHash = hash(raw);
		const id = `import_${hash(`${this.store.project.id}:${sourceHash}`).slice(0, 24)}`;
		return this.store.exclusive(() => {
			mkdirSync(this.dir, { recursive: true, mode: 0o700 });
			if (existsSync(this.path(id))) return this.get(id);
			const inputPath = join(this.dir, `${id}.source.jsonl`);
			// Preserve the byte-for-byte selected transcript before publishing pending work.
			writeFileAtomicSync(inputPath, raw, { mode: 0o600, fsync: true });
			const lines = raw.split("\n");
			const chunks: ImportChunk[] = [];
			const excludedLines: number[] = [];
			let evidenceRecords = 0;
			for (let index = 0; index < lines.length; index++) {
				if (!lines[index].trim()) continue;
				let row: Record<string, unknown>;
				try {
					row = record(JSON.parse(lines[index]));
				} catch {
					throw new Error(`Invalid session JSON at line ${index + 1}; preserved input at ${inputPath}`);
				}
				const msg = row.type === "message" ? row.message : undefined;
				const evidence = msg
					? messageEvidence(
							msg as AgentMessage,
							`${pathToFileURL(inputPath).href}#L${index + 1}`,
							typeof row.id === "string" ? row.id : undefined,
						)
					: undefined;
				if (!evidence || evidence.origin === "derived") {
					excludedLines.push(index + 1);
					continue;
				}
				evidenceRecords++;
				// Oversized code/logs are split losslessly; source ID and character coverage remain explicit.
				const chunkBudget = Math.min(settings.maxImportChunkChars, 75000);
				const textBudget = chunkBudget - serializeEvidence([{ ...evidence, text: "" }]).length - 64;
				if (textBudget < 1) throw new Error("Source reference exceeds chunk budget; increase maxImportChunkChars");
				for (let offset = 0; offset < evidence.text.length; offset += textBudget) {
					const part = {
						...evidence,
						id: `${evidence.id}:${offset}`,
						text: evidence.text.slice(offset, offset + textBudget),
					};
					const previous = chunks.at(-1);
					if (
						previous &&
						serializeEvidence([...previous.records, part], Number.MAX_SAFE_INTEGER).length <= chunkBudget
					) {
						previous.records.push(part);
						previous.endLine = index + 1;
					} else chunks.push({ records: [part], startLine: index + 1, endLine: index + 1 });
				}
			}
			const job: ImportJob = {
				schema: 1,
				id,
				source,
				sourceHash,
				status: "pending",
				chunks,
				nextChunk: 0,
				expectedRevision: this.store.read().memory.revision,
				coverage: { lines: lines.length, evidenceRecords, excludedLines },
				usage: { input: 0, output: 0 },
			};
			writeJson(this.path(id), job);
			return job;
		});
	}
	async run(id: string, extract: MemoryExtractor, signal?: AbortSignal): Promise<ImportJob> {
		mkdirSync(this.dir, { recursive: true, mode: 0o700 });
		// One extraction worker per project across processes, including restart recovery.
		const release = await lock(this.dir, { stale: 30000, retries: 0 });
		let job: ImportJob | undefined;
		try {
			job = this.get(id);
			if (["applied", "preview"].includes(job.status)) return job;
			job.status = "running";
			delete job.error;
			writeJson(this.path(id), job);
			let processed = 0;
			while (job.nextChunk < job.chunks.length && processed < this.store.settings().maxImportChunksPerRun) {
				signal?.throwIfAborted();
				const result = await extract(job.chunks[job.nextChunk].records);
				job.chunks[job.nextChunk].proposal = result.proposal;
				job.usage.input += result.input;
				job.usage.output += result.output;
				job.nextChunk++;
				processed++;
				writeJson(this.path(id), job);
			}
			job.status = job.nextChunk === job.chunks.length ? "preview" : "pending";
			writeJson(this.path(id), job);
			return job;
		} catch (error) {
			if (job) {
				job = { ...job, status: "failed", error: error instanceof Error ? error.message : String(error) };
				writeJson(this.path(id), job);
			}
			throw error;
		} finally {
			await release();
		}
	}
	async apply(id: string, expectedRevision: number): Promise<ImportJob> {
		const release = await lock(this.dir, { stale: 30000, retries: 0 });
		try {
			const job = this.get(id);
			if (job.status === "applied") return job;
			if (job.status !== "preview")
				throw new Error("Import must finish extraction and be previewed before applying");
			if (!job.acceptedProposal) {
				const edits = job.chunks.flatMap((chunk) => chunk.proposal?.edits ?? []);
				if (edits.some((edit) => edit.action !== "create" || edit.kind !== "memory"))
					throw new Error("Imports only create memories");
				const unique = [
					...new Map(edits.map((edit) => [hash(`${edit.kind}:${edit.content?.trim()}`), edit])).values(),
				];
				const current = this.store.read();
				const newEdits = unique.filter(
					(edit) =>
						!Object.values(current.entries.memory).some(
							(entry) => entry.content.trim() === edit.content?.trim() && !entry.metadata.supersededBy,
						),
				);
				job.acceptedProposal = {
					summary: `Import ${id}`,
					rationale: `Selected session ${job.sourceHash}`,
					expectedOutcome: "Recover selected project knowledge with original evidence",
					edits: newEdits,
				};
				// Persist the exact accepted operation before applying it; crash retries use the same fingerprint.
				writeJson(this.path(id), job);
			}
			await this.store.apply(job.acceptedProposal, { eventId: id, expectedRevision });
			job.status = "applied";
			writeJson(this.path(id), job);
			return job;
		} finally {
			await release();
		}
	}
}
