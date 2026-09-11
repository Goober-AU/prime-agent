import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import type { HarnessEntry } from "../refinement/refinement.js";
import { hash } from "./evidence.js";
import {
	emptyDocument,
	type MemoryDocument,
	type MemoryStore,
	readJson,
	record,
	validateDocument,
	writeJson,
} from "./store.js";

export interface SharedWrite {
	id: string;
	revision: number;
	entries: HarnessEntry[];
	remove: string[];
}
interface SharedCache {
	schema: 1;
	url: string;
	revision: number;
	state: MemoryDocument;
	pending: SharedWrite[];
	connected: boolean;
	error?: string;
}
export class MemorySharing {
	private readonly path: string;
	constructor(readonly store: MemoryStore) {
		this.path = join(store.dir, "shared.json");
	}
	cache(): SharedCache {
		const url = this.store.settings().shared?.url ?? "";
		const cached = existsSync(this.path) ? (readJson(this.path) as SharedCache) : undefined;
		// Changing or detaching an endpoint immediately invalidates its cached assets.
		if (cached?.url === url && url)
			return { ...cached, state: validateDocument(cached.state, this.store.project.id) };
		return {
			schema: 1,
			url,
			revision: 0,
			state: emptyDocument(this.store.project.id),
			pending: [],
			connected: false,
		};
	}
	async queue(ids: string[], remove: string[] = []): Promise<SharedWrite> {
		return this.store.exclusive(() => {
			if (!this.store.settings().shared) throw new Error("Configure sharing for this project first");
			const doc = this.store.read();
			const entries = ids.map((id) => {
				const entry = doc.entries.memory[id];
				if (!entry || entry.metadata.hostId) throw new Error(`Only project memories can be shared: ${id}`);
				return entry;
			});
			const cached = this.cache();
			if (cached.pending.length) throw new Error("Sync or resolve the pending shared write first");
			const revision = cached.revision;
			const write = {
				id: `share_${hash(JSON.stringify({ revision, entries, remove })).slice(0, 32)}`,
				revision,
				entries,
				remove,
			};
			cached.pending.push(write);
			this.store.backup(doc);
			writeJson(this.path, cached);
			return write;
		});
	}
	async sync(): Promise<SharedCache> {
		return this.store.exclusive(async () => {
			const config = this.store.settings().shared;
			if (!config) throw new Error("Sharing is not configured");
			const cached = this.cache();
			const token = readFileSync(config.tokenFile, "utf8").trim();
			if (token.length < 32) throw new Error("Shared memory token must contain at least 32 characters");
			const request = async (write?: SharedWrite) => {
				const response = await fetch(`${config.url}/projects/${this.store.project.id}`, {
					method: write ? "POST" : "GET",
					redirect: "error",
					signal: AbortSignal.timeout(5000),
					headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
					body: write ? JSON.stringify(write) : undefined,
				});
				if (!response.ok)
					throw new Error(
						response.status === 409
							? "Shared revision conflict; inspect remote state and requeue explicitly"
							: `Shared memory HTTP ${response.status}`,
					);
				let body = "";
				const decoder = new TextDecoder();
				if (!response.body) throw new Error("Empty shared memory response");
				const reader = response.body.getReader();
				try {
					for (;;) {
						const next = await reader.read();
						if (next.done) break;
						body += decoder.decode(next.value, { stream: true });
						if (Buffer.byteLength(body) > 8 * 1024 * 1024) throw new Error("Shared memory response too large");
					}
				} finally {
					await reader.cancel();
				}
				return validateDocument(JSON.parse(body + decoder.decode()), this.store.project.id);
			};
			try {
				for (const pending of cached.pending) {
					const state = await request(pending);
					cached.state = state;
					cached.revision = state.memory.revision;
				}
				cached.pending = [];
				cached.state = await request();
				cached.revision = cached.state.memory.revision;
				cached.connected = true;
				delete cached.error;
			} catch (error) {
				cached.connected = false;
				cached.error = error instanceof Error ? error.message : String(error);
			}
			writeJson(this.path, cached);
			return cached;
		});
	}
	async discardPending(): Promise<void> {
		await this.store.exclusive(() => {
			const cached = this.cache();
			writeJson(join(this.store.dir, `shared-pending-${Date.now()}.json`), cached.pending);
			cached.pending = [];
			writeJson(this.path, cached);
		});
	}
}
export function validateSharedWrite(value: unknown): SharedWrite {
	const data = record(value);
	if (
		typeof data.id !== "string" ||
		!/^share_[a-f0-9]{32}$/.test(data.id) ||
		!Number.isSafeInteger(data.revision) ||
		Number(data.revision) < 0 ||
		!Array.isArray(data.entries) ||
		!Array.isArray(data.remove) ||
		data.entries.length > 100 ||
		data.remove.length > 100
	)
		throw new Error("Invalid shared write");
	for (const id of data.remove)
		if (typeof id !== "string" || !/^[A-Za-z0-9_-]{1,160}$/.test(id)) throw new Error("Invalid removal ID");
	return value as SharedWrite;
}
