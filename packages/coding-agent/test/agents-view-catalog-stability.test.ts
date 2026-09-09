import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AgentConnectionSavedSessionInfo } from "../src/modes/agent-connection/types.js";
import { AgentsViewMode, type AgentsViewPersistentState } from "../src/modes/agents-view/agents-view-mode.js";
import type { AgentsViewRow } from "../src/modes/agents-view/agents-view-state.js";
import { listDaemonSavedSessions } from "../src/modes/daemon/saved-session-catalog.js";

vi.mock("../src/modes/daemon/saved-session-catalog.js", () => ({
	listDaemonSavedSessions: vi.fn(),
	deleteDaemonSavedSession: vi.fn(),
	renameDaemonSavedSession: vi.fn(),
}));

function saved(id: string): AgentConnectionSavedSessionInfo {
	return {
		id,
		path: `/synthetic/${id}.jsonl`,
		cwd: "/synthetic",
		created: new Date(0),
		modified: new Date(0),
		messageCount: 2,
		firstMessage: id,
		allMessagesText: id,
	};
}

function viewFixture(previous: AgentConnectionSavedSessionInfo[] = []) {
	const state: AgentsViewPersistentState = { savedSessions: previous, lastSuccessfulSavedSessions: previous };
	const view = Object.assign(Object.create(AgentsViewMode.prototype) as object, {
		persistentState: state,
		savedSessions: previous,
		lastSuccessfulSavedSessions: previous,
		savedCatalogGeneration: 0,
		savedCatalogProgress: 0,
		savedCatalogRefreshPending: false,
		stopped: false,
		daemonShutdownReceived: false,
		allRows: [],
		requireClient: () => ({}),
		getSavedSessionCatalogContext: () => ({ cwd: "/synthetic" }),
		reconcileCatalogs: vi.fn(),
		resolveMissingSelectionAnchor: vi.fn(),
		rearmSavedSearchFetch: vi.fn(),
		setStatusMessage: vi.fn(),
		ui: { requestRender: vi.fn() },
	});
	const refresh = (options = {}) =>
		(Reflect.get(AgentsViewMode.prototype, "refreshSavedSessions") as (options: object) => Promise<boolean>).call(
			view,
			options,
		);
	return { view, state, refresh };
}

describe("saved catalog publication", () => {
	beforeEach(() => vi.clearAllMocks());
	it("shows progress without rebuilding a growing catalog for each record", async () => {
		const previous = [saved("cached")];
		const fixture = viewFixture(previous);
		const sessions = Array.from({ length: 300 }, (_, i) => saved(`session-${i}`));
		vi.mocked(listDaemonSavedSessions).mockImplementationOnce(async (_client, _context, _scope, options) => {
			for (const session of sessions) options?.onSession?.(session);
			expect(fixture.view.reconcileCatalogs).not.toHaveBeenCalled();
			expect(fixture.state.savedSessions).toBe(previous);
			expect(fixture.view.savedCatalogProgress).toBe(300);
			const counts = (Reflect.get(AgentsViewMode.prototype, "getAgentCountsText") as () => string).call(
				fixture.view,
			);
			expect(counts).toContain("loading saved chats (300)");
			return sessions;
		});
		await expect(fixture.refresh()).resolves.toBe(true);
		expect(fixture.view.reconcileCatalogs).toHaveBeenCalledOnce();
		expect(fixture.state.savedSessions).toBe(sessions);
		expect(fixture.state.savedCatalogLoaded).toBe(true);
		expect(fixture.view.savedCatalogRefreshPending).toBe(false);
	});
	it("publishes all 234 saved chats once and makes them visible despite the legacy collapse default", async () => {
		const fixture = viewFixture();
		fixture.state.inactiveExpanded = false;
		Object.assign(fixture.view, {
			lastListedSummaries: [],
			heartbeats: [],
			inactiveAgentIdentities: new Set<string>(),
			expandedSubagentParents: new Set<string>(),
			programShownParents: new Set<string>(),
			editor: { getText: () => "" },
			rows: [],
			selectedIndex: 0,
			withPendingDeleteSession: (sessions: unknown[]) => sessions,
			getFilteredRecords: () => Reflect.get(fixture.view, "scopedRecords"),
			applyPendingAncestorExpansion: vi.fn(),
			restoreSelection: vi.fn(),
		});
		fixture.view.reconcileCatalogs.mockImplementation(() => {
			(Reflect.get(AgentsViewMode.prototype, "reconcileCatalogs") as () => void).call(fixture.view);
		});
		const sessions = Array.from({ length: 234 }, (_, index) => saved(`saved-${index}`));
		vi.mocked(listDaemonSavedSessions).mockImplementationOnce(async (_client, _context, _scope, options) => {
			for (const session of sessions) {
				options?.onSession?.(session);
				expect(fixture.view.reconcileCatalogs).not.toHaveBeenCalled();
			}
			expect(fixture.view.savedCatalogProgress).toBe(234);
			return sessions;
		});
		await expect(fixture.refresh()).resolves.toBe(true);
		expect(fixture.view.reconcileCatalogs).toHaveBeenCalledOnce();
		const rows = Reflect.get(fixture.view, "rows") as AgentsViewRow[];
		expect(rows).toHaveLength(234);
		expect(rows.every((row) => row.section === "inactive" && row.selectable)).toBe(true);
		expect(new Set(rows.map((row) => row.summary.sessionId))).toEqual(new Set(sessions.map((session) => session.id)));
		(Reflect.get(AgentsViewMode.prototype, "rebuildRows") as () => void).call(fixture.view);
		expect(Reflect.get(fixture.view, "rows")).toHaveLength(234);
		expect(fixture.state.savedSessions).toBe(sessions);
	});
	it.each(["stopped", "daemonShutdownReceived"])("does not publish late results after %s", async (field) => {
		const fixture = viewFixture();
		vi.mocked(listDaemonSavedSessions).mockImplementationOnce(async (_client, _context, _scope, options) => {
			Reflect.set(fixture.view, field, true);
			options?.onSession?.(saved("late"));
			return [saved("late")];
		});
		await expect(fixture.refresh()).resolves.toBe(false);
		expect(fixture.view.reconcileCatalogs).not.toHaveBeenCalled();
		expect(fixture.state.savedSessions).toEqual([]);
		expect(fixture.view.savedCatalogProgress).toBe(0);
	});
	it("rejects a stale generation after a newer scan completes", async () => {
		const fixture = viewFixture();
		let resolveOld: (sessions: AgentConnectionSavedSessionInfo[]) => void = () => {};
		vi.mocked(listDaemonSavedSessions).mockImplementationOnce(
			() =>
				new Promise((resolve) => {
					resolveOld = resolve;
				}),
		);
		const old = fixture.refresh();
		const newest = [saved("new")];
		vi.mocked(listDaemonSavedSessions).mockResolvedValueOnce(newest);
		await fixture.refresh();
		resolveOld([saved("old")]);
		await expect(old).resolves.toBe(false);
		expect(fixture.state.savedSessions).toBe(newest);
		expect(fixture.view.reconcileCatalogs).toHaveBeenCalledOnce();
	});
	it("keeps the complete cache and makes an initial scan failure visible", async () => {
		const previous = [saved("cached")];
		const fixture = viewFixture(previous);
		vi.mocked(listDaemonSavedSessions).mockRejectedValueOnce(new Error("catalog unavailable"));
		await expect(fixture.refresh({ preserveStatusOnError: true })).resolves.toBe(false);
		expect(fixture.state.savedSessions).toBe(previous);
		expect(fixture.view.setStatusMessage).toHaveBeenCalledWith("Failed to load saved sessions: catalog unavailable");
		expect(fixture.view.rearmSavedSearchFetch).toHaveBeenCalledOnce();
	});
});
