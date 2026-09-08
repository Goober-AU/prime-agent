import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

const packScript = fileURLToPath(new URL("../../../scripts/pack-prime-agent-release.mjs", import.meta.url));
const version = "0.9.3-telemusai.main.10.abcdef012";
const packages = [
	{ dir: "ai", source: "@earendil-works/pi-ai", name: "prime-agent-ai", dependencies: {} },
	{ dir: "tui", source: "@earendil-works/pi-tui", name: "prime-agent-tui", dependencies: {} },
	{
		dir: "agent",
		source: "@earendil-works/pi-agent-core",
		name: "prime-agent-core",
		dependencies: { "@earendil-works/pi-ai": "^0.9.3" },
	},
	{
		dir: "coding-agent",
		source: "@earendil-works/pi-coding-agent",
		name: "prime-agent",
		dependencies: {
			"@earendil-works/pi-ai": "^0.9.3",
			"@earendil-works/pi-tui": "^0.9.3",
			"@earendil-works/pi-agent-core": "^0.9.3",
		},
	},
];

describe("release packaging", () => {
	let root: string;
	beforeEach(() => {
		root = mkdtempSync(join(tmpdir(), "prime-release-pack-"));
		mkdirSync(join(root, "scripts"));
		copyFileSync(packScript, join(root, "scripts", "pack-prime-agent-release.mjs"));
		writeFileSync(join(root, "package.json"), JSON.stringify({ private: true }));
		for (const pkg of packages) {
			const directory = join(root, "packages", pkg.dir);
			mkdirSync(join(directory, "dist"), { recursive: true });
			writeFileSync(join(directory, "dist", "index.js"), "export const value = 42;\n");
			writeFileSync(
				join(directory, "package.json"),
				JSON.stringify({
					name: pkg.source,
					version: "0.9.3",
					type: "module",
					files: ["dist"],
					dependencies: pkg.dependencies,
				}),
			);
		}
	});
	afterEach(() => rmSync(root, { recursive: true, force: true }));

	it.each([
		{
			channel: "main",
			args: ["--github-repository", "telemusai/prime-agent"],
			url: `https://github.com/telemusai/prime-agent/releases/download/v${version}`,
		},
		{
			channel: "stable",
			args: ["--base-url", "https://releases.example.test"],
			url: `https://releases.example.test/releases/v${version}`,
		},
		{
			channel: "beta",
			args: ["--base-url", "https://releases.example.test"],
			url: `https://releases.example.test/releases/v${version}`,
		},
	])(
		"packs $channel assets with resolvable internal dependencies and matching checksums",
		({ channel, args, url }) => {
			const out = join(root, "packages", "coding-agent", "release", "test");
			execFileSync(
				process.execPath,
				[
					join(root, "scripts", "pack-prime-agent-release.mjs"),
					...args,
					"--channel",
					channel,
					"--version",
					version,
					"--out-dir",
					out,
				],
				{ cwd: root, encoding: "utf8", timeout: 25000 },
			);
			const manifest = JSON.parse(
				readFileSync(join(out, "artifacts", channel === "stable" ? "latest.json" : `${channel}.json`), "utf8"),
			);
			expect(manifest).toMatchObject({ channel, version: `v${version}`, package: "prime-agent" });
			expect(manifest.tarball).toBe(
				channel === "main"
					? `${url}/prime-agent-${version}.tgz`
					: `releases/v${version}/prime-agent-${version}.tgz`,
			);
			const checksums = readFileSync(join(out, "artifacts", "SHA256SUMS"), "utf8");
			for (const pkg of packages) {
				const name = `${pkg.name}-${version}.tgz`;
				const tarball = join(out, "artifacts", name);
				expect(existsSync(tarball)).toBe(true);
				const digest = createHash("sha256").update(readFileSync(tarball)).digest("hex");
				expect(checksums).toContain(`${digest}  ${name}\n`);
				expect(manifest.tarballs).toContainEqual({ package: pkg.name, file: name, sha256: digest });
				const staged = JSON.parse(readFileSync(join(out, "packages", pkg.dir, "package.json"), "utf8"));
				expect(staged).toMatchObject({ name: pkg.name, version });
				for (const dependency of Object.keys(pkg.dependencies)) {
					const target = packages.find((candidate) => candidate.source === dependency)!;
					expect(staged.dependencies[dependency]).toBe(`${url}/${target.name}-${version}.tgz`);
				}
			}
		},
	);
});
