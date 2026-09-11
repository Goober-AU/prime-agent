import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import type { Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { collectEvidence, serializeEvidence } from "../src/core/memory/evidence.js";
import { MemoryJobs } from "../src/core/memory/jobs.js";
import { normalizeRemote, projectIdentity } from "../src/core/memory/project.js";
import { createMemoryServer } from "../src/core/memory/server.js";
import { MemoryService } from "../src/core/memory/service.js";
import { emptyDocument, MemoryStore, readJson, writeJson } from "../src/core/memory/store.js";
import { type RefinementProposal, saveHarnessState } from "../src/core/refinement/refinement.js";

const paths: string[] = [];
const servers: Server[] = [];
function fixture() {
	const root = mkdtempSync(join(tmpdir(), "prime-memory-"));
	paths.push(root);
	const cwd = join(root, "repo");
	mkdirSync(cwd);
	const agentDir = join(root, "agent");
	const service = new MemoryService(cwd, agentDir);
	return { root, cwd, agentDir, service, store: service.store };
}
function proposal(id: string, content = `Content ${id}`): RefinementProposal {
	return {
		summary: id,
		rationale: "test evidence",
		expectedOutcome: "correct recall",
		edits: [{ action: "create", kind: "memory", id, title: id, content }],
	};
}
afterEach(async () => {
	for (const server of servers.splice(0)) {
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
	}
	for (const path of paths.splice(0)) rmSync(path, { recursive: true, force: true });
});

describe("project identity and evidence", () => {
	it("normalizes authenticated URLs without leaking credentials and retains identity through remote rename", () => {
		const { cwd, agentDir } = fixture();
		expect(normalizeRemote("https://secret@github.com/telemusai/optimus-agent.git")).toBe(
			normalizeRemote("git@github.com:telemusai/optimus-agent.git"),
		);
		execFileSync("git", ["init", cwd], { stdio: "ignore" });
		execFileSync("git", ["-C", cwd, "remote", "add", "origin", "https://github.com/telemusai/prime.git"]);
		const first = projectIdentity(cwd, agentDir);
		execFileSync("git", ["-C", cwd, "remote", "set-url", "origin", "git@github.com:telemusai/optimus-agent.git"]);
		expect(projectIdentity(cwd, agentDir).id).toBe(first.id);
		expect(projectIdentity(cwd, agentDir, "project_explicit").id).toBe("project_explicit");
	});
	it("excludes synthetic memory and preserves real origins and complete code", () => {
		const records = collectEvidence([
			{ role: "custom", customType: "harness_digest", content: "false recalled fact", display: false, timestamp: 1 },
			{ role: "user", content: "Correction: use Sydney\n```py\nx = 123\n```", timestamp: 2 },
			{
				role: "toolResult",
				toolCallId: "c1",
				toolName: "ipython",
				content: [{ type: "text", text: "[memory data; not new evidence] prior claim" }],
				isError: false,
				timestamp: 3,
			},
		]);
		expect(records).toHaveLength(2);
		expect(records.map((r) => r.origin)).toEqual(["user", "derived"]);
		const text = serializeEvidence(records);
		expect(text).not.toContain("false recalled fact");
		expect(text).toContain("x = 123");
		expect(text).toContain(records[0].id);
		expect(serializeEvidence(records, 400).length).toBeLessThanOrEqual(400);
	});
});

describe("authoritative scoped memory", () => {
	it("serializes competing revisions, deduplicates retries and refuses different payload replay", async () => {
		const { store } = fixture();
		const create = proposal("one");
		const competing = await Promise.allSettled([
			store.apply(create, { eventId: "op_one", expectedRevision: 0 }),
			store.apply(proposal("two"), { eventId: "op_two", expectedRevision: 0 }),
		]);
		expect(competing.filter((r) => r.status === "fulfilled")).toHaveLength(1);
		const winner = store.read().memory.history[0];
		const original = winner.id === "op_one" ? create : proposal("two");
		expect((await store.apply(original, { eventId: winner.id, expectedRevision: 0 })).id).toBe(winner.id);
		await expect(store.apply(proposal("different"), { eventId: winner.id, expectedRevision: 1 })).rejects.toThrow(
			"different content",
		);
		await expect(
			store.apply(proposal("duplicate", original.edits[0].content), { eventId: "duplicate", expectedRevision: 1 }),
		).rejects.toThrow("duplicate");
		expect(store.read().memory.revision).toBe(1);
	});
	it("searches beyond overview limits across allowed stores and bounds recalled content", async () => {
		const { store, service, agentDir, root } = fixture();
		const state = emptyDocument(store.project.id);
		for (let index = 0; index < 20; index++)
			await store.apply(
				proposal(
					`entry_${index}`,
					index === 19 ? `Sydney Astra ${"details ".repeat(2000)}` : `Oregon item ${index}`,
				),
				{ eventId: `op_${index}`, expectedRevision: index },
			);
		state.entries.memory.global = {
			...store.read().entries.memory.entry_0,
			id: "global",
			title: "Global Astra",
			metadata: {},
		};
		saveHarnessState(join(agentDir, "harness"), state);
		const other = new MemoryStore(agentDir, { id: "project_other", root, aliases: [] });
		await other.apply(proposal("secret", "Sydney private"), { eventId: "private", expectedRevision: 0 });
		expect(service.search("Sydney")[0].entry.id).toBe("entry_19");
		expect(service.search("Astra").some((hit) => hit.scope === "global")).toBe(true);
		expect(service.search("private")).toHaveLength(0);
		await store.configure({ maxRecallChars: 800, maxRecallEntries: 2 });
		const recall = service.recall("Sydney");
		expect(recall.chars).toBeLessThanOrEqual(800);
		expect(recall.ids).toHaveLength(1);
		await store.configure({ recall: false });
		expect(service.recall("Sydney").text).toBe("");
	});
	it("keeps recall available while learning is paused and detects changed source files", async () => {
		const { store, service, root } = fixture();
		const file = join(root, "runbook.md");
		writeFileSync(file, "original");
		const source = await service.request("source", { path: file });
		await service.request("apply", {
			eventId: "fact",
			revision: 0,
			sources: [source],
			proposal: proposal("runbook", "Sydney deployment"),
		});
		await store.configure({ learning: false });
		expect(service.recall("Sydney").ids).toHaveLength(1);
		await expect(
			store.apply(proposal("auto"), { eventId: "auto", expectedRevision: 1, automatic: true }),
		).rejects.toThrow("paused");
		writeFileSync(file, "changed");
		expect(service.recall("Sydney").ids).toHaveLength(0);
		expect(await service.request("search", { query: "Sydney" })).toMatchObject([{ freshness: "stale" }]);
	});
	it("backs up history, validates restore, preserves delivery receipts and supports rollback", async () => {
		const { store } = fixture();
		await store.apply(proposal("original"), { eventId: "one", expectedRevision: 0 });
		const backup = store.backup();
		await store.apply(proposal("new"), { eventId: "two", expectedRevision: 1 });
		await store.rollback("two", 2);
		expect(store.read().entries.memory.new).toBeUndefined();
		await store.restore(backup);
		expect(store.read().memory.revision).toBe(4);
		await expect(store.apply(proposal("new"), { eventId: "two", expectedRevision: 4 })).rejects.toThrow(
			"before restore",
		);
		const path = join(store.dir, "backups", `${backup}.json`);
		const broken = readJson(path) as { sha256: string };
		broken.sha256 = "bad";
		writeJson(path, broken);
		await expect(store.restore(backup)).rejects.toThrow("checksum");
		expect(store.read().memory.revision).toBe(4);
	});
	it("restores host isolation and removes supersession metadata when rolling back", async () => {
		const { store, service } = fixture();
		await store.apply(proposal("host", "host configuration"), {
			eventId: "host_create",
			expectedRevision: 0,
			host: true,
		});
		await store.apply(
			{ ...proposal("delete"), edits: [{ action: "delete", kind: "memory", id: "host" }] },
			{ eventId: "host_delete", expectedRevision: 1 },
		);
		await store.rollback("host_delete", 2);
		expect(store.read().entries.memory.host.metadata.hostId).toBe(store.hostId);
		await expect(service.sharing.queue(["host"])).rejects.toThrow();
		await store.apply(
			{
				...proposal("supersede"),
				edits: [
					{
						action: "update",
						kind: "memory",
						id: "host",
						title: "host",
						content: "host configuration",
						metadata: { supersededBy: "replacement" },
					},
				],
			},
			{ eventId: "supersede", expectedRevision: 3 },
		);
		expect(service.search("configuration")).toHaveLength(0);
		await store.rollback("supersede", 4);
		expect(store.read().entries.memory.host.metadata.supersededBy).toBeUndefined();
		expect(service.search("configuration")).toHaveLength(1);
	});
	it("fails closed on corrupt authoritative state", async () => {
		const { store } = fixture();
		mkdirSync(store.dir, { recursive: true });
		writeFileSync(store.path, "broken");
		await expect(store.apply(proposal("one"), { eventId: "one", expectedRevision: 0 })).rejects.toThrow();
		expect(readFileSync(store.path, "utf8")).toBe("broken");
	});
});

describe("history recovery", () => {
	it("preserves raw code, reports coverage, resumes failed chunks and requires a completed preview", async () => {
		const { store, root } = fixture();
		await store.configure({ maxImportChunkChars: 1000 });
		const path = join(root, "session.jsonl");
		const raw = [
			JSON.stringify({ type: "session", id: "s", cwd: root }),
			JSON.stringify({
				type: "message",
				id: "u",
				message: { role: "user", timestamp: 1, content: `Keep this code ${"print('abc')\n".repeat(130)}` },
			}),
			JSON.stringify({ type: "custom_message", customType: "harness_digest", content: "ignored" }),
		].join("\n");
		writeFileSync(path, raw);
		const jobs = new MemoryJobs(store);
		const prepared = await jobs.prepare(path);
		expect(readFileSync(join(jobs.dir, `${prepared.id}.source.jsonl`), "utf8")).toBe(raw);
		expect(prepared.coverage.excludedLines).toEqual([1, 3]);
		await expect(jobs.apply(prepared.id, 0)).rejects.toThrow("preview");
		let calls = 0;
		await expect(
			jobs.run(prepared.id, async () => {
				if (++calls === 2) throw new Error("offline");
				return { proposal: proposal("recovered"), input: 20, output: 5 };
			}),
		).rejects.toThrow("offline");
		expect(jobs.get(prepared.id).nextChunk).toBe(1);
		const resumed = await new MemoryJobs(store).run(prepared.id, async () => ({
			proposal: { ...proposal("empty"), edits: [] },
			input: 10,
			output: 2,
		}));
		expect(resumed.status).toBe("preview");
		expect(resumed.usage).toEqual({
			input: 20 + (prepared.chunks.length - 1) * 10,
			output: 5 + (prepared.chunks.length - 1) * 2,
		});
		await jobs.apply(prepared.id, 0);
		expect(store.read().entries.memory.recovered).toBeDefined();
		expect((await jobs.prepare(path)).status).toBe("applied");
		const acknowledged = jobs.get(prepared.id);
		acknowledged.status = "preview";
		writeJson(join(jobs.dir, `${prepared.id}.json`), acknowledged);
		await jobs.apply(prepared.id, 0);
		expect(store.read().memory.revision).toBe(1);
	});
});

describe("cross-machine scope and bounded work", () => {
	it("keeps host facts private after copying a project store and remaps repository source paths", async () => {
		const { store, service, cwd, root } = fixture();
		const file = join(cwd, "runbook.md");
		writeFileSync(file, "authoritative runbook");
		const source = await service.request("source", { path: file });
		await service.request("apply", {
			eventId: "portable",
			revision: 0,
			sources: [source],
			proposal: proposal("portable", "Sydney portable docs"),
		});
		await store.apply(proposal("private", "host-only Sydney fact"), {
			eventId: "host",
			expectedRevision: 1,
			host: true,
		});
		const peerRoot = join(root, "peer-repo");
		mkdirSync(peerRoot);
		writeFileSync(join(peerRoot, "runbook.md"), "authoritative runbook");
		const peerDir = join(root, "peer-agent");
		projectIdentity(peerRoot, peerDir, store.project.id);
		const peer = new MemoryService(peerRoot, peerDir);
		mkdirSync(peer.store.dir, { recursive: true });
		writeJson(peer.store.path, store.read());
		expect(peer.store.hostId).not.toBe(store.hostId);
		expect(peer.search("host-only")).toHaveLength(0);
		rmSync(file);
		expect(peer.recall("portable").ids).toHaveLength(1);
		writeFileSync(join(peerRoot, "runbook.md"), "changed");
		expect(peer.recall("portable").ids).toHaveLength(0);
	});
	it("limits extraction calls per invocation and does not silently skip source records", async () => {
		const { store, root } = fixture();
		await store.configure({ maxImportChunkChars: 1000, maxImportChunksPerRun: 1 });
		const path = join(root, "long.jsonl");
		const messages = Array.from({ length: 30 }, (_, i) => ({
			type: "message",
			id: `u${i}`,
			message: { role: "user", timestamp: i, content: `durable fact ${i}` },
		}));
		writeFileSync(path, messages.map((m) => JSON.stringify(m)).join("\n"));
		const jobs = new MemoryJobs(store);
		const prepared = await jobs.prepare(path);
		let calls = 0;
		const result = await jobs.run(prepared.id, async () => {
			calls++;
			return { proposal: { ...proposal("none"), edits: [] }, input: 1, output: 1 };
		});
		expect(calls).toBe(1);
		expect(result.status).toBe("pending");
		expect(result.nextChunk).toBe(1);
		expect(prepared.chunks.flatMap((chunk) => chunk.records)).toHaveLength(30);
		for (const chunk of prepared.chunks) expect(serializeEvidence(chunk.records).length).toBeLessThanOrEqual(1000);
	});
});

describe("optional shared authority", () => {
	it("authenticates, rejects host publication, survives offline reads and enforces cross-client revisions", async () => {
		const { service, store, root } = fixture();
		const token = "synthetic-memory-token-".repeat(3);
		const tokenFile = join(root, "token");
		writeFileSync(tokenFile, token);
		const server = createMemoryServer(join(root, "shared"), token);
		servers.push(server);
		await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
		const address = server.address();
		if (!address || typeof address === "string") throw new Error("No address");
		const url = `http://127.0.0.1:${address.port}`;
		expect((await fetch(`${url}/projects/${store.project.id}`)).status).toBe(401);
		await store.configure({ shared: { url, tokenFile } });
		await store.apply(proposal("public", "Sydney shared fact"), { eventId: "public", expectedRevision: 0 });
		await store.apply(proposal("host", "local machine path"), { eventId: "host", expectedRevision: 1, host: true });
		await expect(service.sharing.queue(["host"])).rejects.toThrow("project memories");
		await service.sharing.queue(["public"]);
		expect((await service.sharing.sync()).connected).toBe(true);
		const otherRoot = join(root, "second-host");
		mkdirSync(otherRoot);
		const peerDir = join(root, "second-agent");
		projectIdentity(otherRoot, peerDir, store.project.id);
		const peer = new MemoryService(otherRoot, peerDir);
		await peer.store.configure({ shared: { url, tokenFile } });
		expect((await peer.sharing.sync()).revision).toBe(1);
		expect(peer.search("Sydney")[0].scope).toBe("shared");
		await service.sharing.queue([], ["public"]);
		await service.sharing.sync();
		await peer.store.apply(proposal("peer", "peer fact"), { eventId: "peer", expectedRevision: 0 });
		await peer.sharing.queue(["peer"]);
		const conflict = await peer.sharing.sync();
		expect(conflict.error).toContain("conflict");
		expect(conflict.pending).toHaveLength(1);
		await peer.sharing.discardPending();
		await peer.sharing.sync();
		expect(peer.search("Sydney")).toHaveLength(0);
		await service.sharing.queue(["public"]);
		await service.sharing.sync();
		await peer.sharing.sync();
		server.closeAllConnections();
		await new Promise<void>((resolve) => server.close(() => resolve()));
		servers.splice(servers.indexOf(server), 1);
		expect((await peer.sharing.sync()).connected).toBe(false);
		expect(peer.search("Sydney")).toHaveLength(1);
	});
	it("rejects insecure endpoints and invalid limits", async () => {
		const { store } = fixture();
		await expect(store.configure({ shared: { url: "http://example.com", tokenFile: "token" } })).rejects.toThrow(
			"HTTPS",
		);
		await expect(store.configure({ maxRecallChars: -1 })).rejects.toThrow("maxRecallChars");
	});
});
