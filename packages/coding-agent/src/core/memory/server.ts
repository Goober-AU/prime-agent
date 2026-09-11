import { timingSafeEqual } from "node:crypto";
import { existsSync, mkdirSync, readFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { lock } from "proper-lockfile";
import { hash } from "./evidence.js";
import { validateSharedWrite } from "./sharing.js";
import { emptyDocument, readJson, validateDocument, writeJson } from "./store.js";

export function createMemoryServer(dir: string, token: string): Server {
	if (token.length < 32) throw new Error("Shared memory token must contain at least 32 characters");
	const expected = Buffer.from(`Bearer ${token}`);
	mkdirSync(dir, { recursive: true, mode: 0o700 });
	return createServer(async (req, res) => {
		res.setHeader("Content-Type", "application/json");
		res.setHeader("Cache-Control", "no-store");
		const supplied = Buffer.from(req.headers.authorization ?? "");
		if (supplied.length !== expected.length || !timingSafeEqual(supplied, expected)) {
			res.writeHead(401).end('{"error":"Unauthorized"}');
			return;
		}
		const match = req.url?.match(/^\/projects\/(project_[A-Za-z0-9_-]{1,80})$/);
		if (!match || !["GET", "POST"].includes(req.method ?? "")) {
			res.writeHead(404).end("{}");
			return;
		}
		const projectId = match[1];
		const projectDir = join(dir, projectId);
		mkdirSync(projectDir, { recursive: true, mode: 0o700 });
		const path = join(projectDir, "harness_state.json");
		try {
			let body = "";
			if (req.method === "POST") {
				const chunks: Buffer[] = [];
				let size = 0;
				for await (const chunk of req) {
					const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
					size += bytes.length;
					if (size > 2 * 1024 * 1024) {
						res.writeHead(413).end("{}");
						return;
					}
					chunks.push(bytes);
				}
				body = Buffer.concat(chunks).toString("utf8");
			}
			const release = await lock(projectDir, { retries: { retries: 20, minTimeout: 10, maxTimeout: 100 } });
			try {
				const state = existsSync(path) ? validateDocument(readJson(path), projectId) : emptyDocument(projectId);
				if (req.method === "POST") {
					const write = validateSharedWrite(JSON.parse(body));
					const fingerprint = hash(JSON.stringify(write));
					const receipt = state.memory.events[write.id];
					if (receipt && receipt !== fingerprint) throw new Error("Reused operation ID");
					if (!receipt) {
						if (state.memory.revision !== write.revision) {
							res.writeHead(409).end('{"error":"Revision conflict"}');
							return;
						}
						const staged = structuredClone(state);
						for (const entry of write.entries) {
							if (
								entry.kind !== "memory" ||
								!/^[A-Za-z0-9_-]{1,160}$/.test(entry.id) ||
								["__proto__", "constructor", "prototype"].includes(entry.id) ||
								entry.metadata?.projectId !== projectId ||
								entry.metadata?.hostId
							)
								throw new Error("Only this project's host-neutral memories can be shared");
							staged.entries.memory[entry.id] = entry;
						}
						for (const id of write.remove) delete staged.entries.memory[id];
						staged.memory.revision++;
						staged.memory.events[write.id] = fingerprint;
						validateDocument(staged, projectId);
						if (Buffer.byteLength(JSON.stringify(staged)) > 8 * 1024 * 1024)
							throw new Error("Shared project exceeds size limit");
						writeJson(join(projectDir, `backup_${state.memory.revision}.json`), state);
						writeJson(path, staged);
						res.end(JSON.stringify(staged));
						return;
					}
				}
				res.end(JSON.stringify(state));
			} finally {
				await release();
			}
		} catch {
			res.writeHead(400).end('{"error":"Invalid memory request or unavailable storage"}');
		}
	});
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
	const [dir, tokenFile, portText = "8799"] = process.argv.slice(2);
	const port = Number(portText);
	if (!dir || !tokenFile || !Number.isInteger(port) || port < 1 || port > 65535)
		throw new Error("Usage: node server.js <state-directory> <token-file> [port]");
	const server = createMemoryServer(dir, readFileSync(tokenFile, "utf8").trim());
	server.requestTimeout = 10000;
	server.listen(port, "127.0.0.1", () => process.stdout.write(`Memory service listening on 127.0.0.1:${port}\n`));
}
