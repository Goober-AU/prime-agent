import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import type { AgentConnectionSavedSessionInfo } from "../src/modes/agent-connection/types.js";
import {
	buildAgentsViewRows,
	filterUnifiedSessions,
	isSubagentSummary,
	reconcileUnifiedSessions,
	scopeToSessionSubtree,
} from "../src/modes/agents-view/agents-view-state.js";
import type { SessionSummary } from "../src/modes/daemon/daemon-session-list.js";

function saved(id: string, depth: number, parent?: string): AgentConnectionSavedSessionInfo {
	return {
		id,
		path: resolve("sessions", `${id}.jsonl`),
		cwd: resolve("project"),
		name: id,
		rlmDepth: depth,
		parentSessionPath: parent ? resolve("sessions", `${parent}.jsonl`) : undefined,
		created: new Date(0),
		modified: new Date(0),
		messageCount: 1,
		firstMessage: id,
		allMessagesText: id,
	};
}

function live(id: string, overrides: Partial<SessionSummary> = {}): SessionSummary {
	return {
		id,
		activeSessionId: id,
		sessionId: id,
		lifecycle: "live",
		activity: "idle",
		isSessionActive: false,
		cwd: resolve("project"),
		isStreaming: false,
		isCompacting: false,
		attachedClients: 0,
		messageCount: 1,
		sessionActions: { queuedCount: 0, steering: [], followUps: [] },
		...overrides,
	};
}

describe("inactive child roots", () => {
	it("retains every main chat and ordinary fork without promoting inactive orphan children", () => {
		const main = Array.from({ length: 234 }, (_, i) => saved(`main-${i}`, 0));
		const records = reconcileUnifiedSessions([], [...main, saved("fork", 0, "main-0"), saved("orphan", 2)]);
		const rows = buildAgentsViewRows(records);
		expect(rows).toHaveLength(235);
		expect(rows.some((row) => row.summary.sessionId === "orphan")).toBe(false);
		expect(records).toHaveLength(236);
		expect(records.find((record) => record.saved?.id === "orphan")).toBeDefined();
	});

	it("keeps children and grandchildren accessible through their parent and in scoped searches", () => {
		const records = reconcileUnifiedSessions(
			[],
			[saved("root", 0), saved("child", 1, "root"), saved("grand", 2, "child")],
		);
		const expanded = new Set(records.map((record) => record.identity));
		const rows = buildAgentsViewRows(records, expanded).filter(
			(row) => row.selectable && row.kind !== "subagent-summary",
		);
		expect(rows.map((row) => [row.summary.sessionId, row.depth])).toEqual([
			["root", 0],
			["child", 1],
			["grand", 2],
		]);
		const scoped = scopeToSessionSubtree(records, { sessionId: "root" });
		expect(
			buildAgentsViewRows(scoped, expanded, new Set(), { sessionId: "root" }).some(
				(row) => row.summary.sessionId === "child",
			),
		).toBe(true);
		expect(filterUnifiedSessions(records, (text) => text.includes("grand"))).toHaveLength(3);
	});

	it("handles child-before-parent arrival without deleting or misclassifying the child", () => {
		const child = saved("child", 1, "root");
		expect(buildAgentsViewRows(reconcileUnifiedSessions([], [child]))).toEqual([]);
		const records = reconcileUnifiedSessions([], [child, saved("root", 0)]);
		const rows = buildAgentsViewRows(records, new Set(records.map((record) => record.identity)));
		expect(rows.find((row) => row.summary.sessionId === "child")?.depth).toBe(1);
	});

	it("uses persisted depth over stale top-level kind and keeps running orphan children visible", () => {
		expect(isSubagentSummary(live("old", { runtimeKind: "top-level", rlmDepth: 2 }))).toBe(true);
		expect(isSubagentSummary(live("fork", { rlmDepth: 0, parentSessionId: "root" }))).toBe(false);
		const child = live("working", { runtimeKind: "top-level", rlmDepth: 1, activity: "working", isStreaming: true });
		expect(buildAgentsViewRows([child])[0]).toMatchObject({ section: "running", summary: { sessionId: "working" } });
	});
});
