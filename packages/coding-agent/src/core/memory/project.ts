import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, realpathSync } from "node:fs";
import { join, resolve } from "node:path";
import { lockSync } from "proper-lockfile";
import { writeFileAtomicSync } from "../../utils/atomic-file.js";
import { hash } from "./evidence.js";

export interface ProjectIdentity {
	id: string;
	root: string;
	aliases: string[];
}
export function normalizeRemote(remote: string): string {
	const value = remote
		.trim()
		.replace(/\.git\/?$/, "")
		.replace(/\/$/, "");
	const scp = value.match(/^(?:[^/@]+@)?([^/:]+):([^/].*)$/);
	if (scp && !value.includes("://") && !/^[A-Za-z]:/.test(value)) return `${scp[1].toLowerCase()}/${scp[2]}`;
	try {
		const url = new URL(value);
		return `${url.hostname.toLowerCase()}${url.port ? `:${url.port}` : ""}${url.pathname}`;
	} catch {
		return value;
	}
}
function git(cwd: string, args: string[]): string | undefined {
	try {
		return (
			execFileSync("git", ["-C", cwd, ...args], {
				encoding: "utf8",
				timeout: 1500,
				stdio: ["ignore", "pipe", "ignore"],
			}).trim() || undefined
		);
	} catch {
		return undefined;
	}
}
export function projectIdentity(cwd: string, agentDir: string, bindId?: string): ProjectIdentity {
	const root = git(cwd, ["rev-parse", "--show-toplevel"]) ?? realpathSync(cwd);
	const common = git(cwd, ["rev-parse", "--path-format=absolute", "--git-common-dir"]) ?? root;
	const remote = git(cwd, ["config", "--get", "remote.origin.url"]);
	const aliases = [`path:${resolve(common)}`, ...(remote ? [`remote:${normalizeRemote(remote)}`] : [])];
	const dir = join(agentDir, "memory");
	mkdirSync(dir, { recursive: true, mode: 0o700 });
	const file = join(dir, "projects.json");
	const release = lockSync(dir, { realpath: true });
	try {
		const registry: Record<string, string> = existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : {};
		const existing = aliases.map((alias) => registry[alias]).filter(Boolean);
		const id = bindId ?? existing[0] ?? `project_${hash(aliases.at(-1)!).slice(0, 24)}`;
		if (!/^project_[a-zA-Z0-9_-]{1,80}$/.test(id))
			throw new Error("Project ID must start with project_ and contain only letters, digits, _ or -");
		if (!bindId && existing.some((value) => value !== id))
			throw new Error("Project aliases conflict; explicitly bind the intended project ID");
		if (aliases.some((alias) => registry[alias] !== id)) {
			for (const alias of aliases) registry[alias] = id;
			writeFileAtomicSync(file, JSON.stringify(registry, null, 2), { mode: 0o600 });
		}
		return { id, root, aliases };
	} finally {
		release();
	}
}
