import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import {
	casSnapshotRootForLegacyPath,
	casSnapshotRootIn,
	manifestPathIn,
	snapshotPathIn,
	snapshotStateExistsIn,
} from "../src/core/kernel/state-snapshot.js";

describe("kernel state snapshot paths", () => {
	it("places legacy and CAS v2 state inside the session artifact directory", () => {
		const artifactDir = "/home/u/.prime/agent/session-artifacts/abc-123";
		expect(snapshotPathIn(artifactDir)).toBe(join(artifactDir, "kernel-state.dill"));
		expect(manifestPathIn(artifactDir)).toBe(join(artifactDir, "kernel-state.json"));
		expect(casSnapshotRootIn(artifactDir)).toBe(join(artifactDir, "kernel-state.v2"));
		expect(casSnapshotRootForLegacyPath(join(artifactDir, "custom.dill"))).toBe(join(artifactDir, "custom.v2"));
	});

	it("treats any v2 root as state instead of silently selecting stale legacy", () => {
		const artifactDir = mkdtempSync(join(tmpdir(), "prime-agent-snapshot-presence-"));
		try {
			expect(snapshotStateExistsIn(artifactDir)).toBe(false);
			mkdirSync(casSnapshotRootIn(artifactDir));
			expect(snapshotStateExistsIn(artifactDir)).toBe(true);
			rmSync(casSnapshotRootIn(artifactDir), { recursive: true });
			writeFileSync(snapshotPathIn(artifactDir), "legacy");
			expect(snapshotStateExistsIn(artifactDir)).toBe(true);
		} finally {
			rmSync(artifactDir, { recursive: true, force: true });
		}
	});
});
