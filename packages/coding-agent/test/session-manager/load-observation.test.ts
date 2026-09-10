import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { SessionManager } from "../../src/core/session-manager.js";

describe("SessionManager load observation", () => {
	it("reports primary transcript bytes and replaces or clears stale observations", async () => {
		const directory = mkdtempSync(join(tmpdir(), "prime-session-load-observation-"));
		try {
			const firstPath = join(directory, "first.jsonl");
			const secondPath = join(directory, "second.jsonl");
			const failedPath = join(directory, "failed.jsonl");
			const firstContent = `${JSON.stringify({
				type: "session",
				version: 3,
				id: "first-session",
				timestamp: "2026-09-10T00:00:00.000Z",
				cwd: directory,
			})}\n`;
			const secondContent = `${JSON.stringify({
				type: "session",
				version: 3,
				id: "second-session-with-a-different-size",
				timestamp: "2026-09-10T00:00:00.000Z",
				cwd: directory,
			})}\n`;
			writeFileSync(firstPath, firstContent);
			writeFileSync(secondPath, secondContent);
			writeFileSync(failedPath, "not-json\n");

			const manager = await SessionManager.openAsync(firstPath);
			expect(manager.getLoadObservation()).toEqual({ readBytes: Buffer.byteLength(firstContent) });

			manager.setSessionFile(secondPath);
			expect(manager.getLoadObservation()).toEqual({ readBytes: Buffer.byteLength(secondContent) });

			manager.setSessionFile(failedPath);
			expect(manager.getLoadObservation()).toBeUndefined();

			manager.setSessionFile(join(directory, "missing.jsonl"));
			expect(manager.getLoadObservation()).toBeUndefined();
		} finally {
			rmSync(directory, { recursive: true, force: true });
		}
	});
});
