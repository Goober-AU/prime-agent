import { mkdtempSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { AgentMessage } from "@earendil-works/pi-agent-core";
import { fauxAssistantMessage } from "@earendil-works/pi-ai";
import { describe, expect, it } from "vitest";
import { SessionManager } from "../src/core/session-manager.js";
import { MAX_HISTORY_RANGE_MESSAGES, slicePinnedSessionHistory } from "../src/modes/daemon/daemon-mode.js";

function user(content: string, timestamp: number): Extract<AgentMessage, { role: "user" }> {
	return { role: "user", content, timestamp };
}

function assistant(content: string, timestamp: number): ReturnType<typeof fauxAssistantMessage> {
	return { ...fauxAssistantMessage(content), timestamp };
}

describe("recent-first session history", () => {
	it("handles empty and sub-window sessions without a range round trip", () => {
		const empty = SessionManager.inMemory("/history").buildSessionHistory();
		expect(empty).toEqual({ messages: [], entryIds: [], tipEntryId: null });

		const manager = SessionManager.inMemory("/history");
		manager.appendMessage(user("one", 1));
		manager.appendMessage(user("two", 2));
		const history = manager.buildSessionHistory();
		const window = slicePinnedSessionHistory(history, {
			representation: "representation-1",
			generation: "generation-small",
		});
		expect(window.messages).toEqual(history.messages);
		expect(window.entryIds).toEqual(history.entryIds);
		expect(window.startIndex).toBe(0);
		expect(window.hasOlder).toBe(false);
	});

	it("keeps pinned branch histories disjoint after a leaf change", () => {
		const manager = SessionManager.inMemory("/history");
		const root = manager.appendMessage(user("root", 1));
		const firstBranchTip = manager.appendMessage(user("first branch", 2));
		const firstBranch = manager.buildSessionHistory(firstBranchTip);
		manager.branch(root);
		const secondBranchTip = manager.appendMessage(user("second branch", 3));
		const secondBranch = manager.buildSessionHistory(secondBranchTip);

		expect(firstBranch.entryIds).toEqual([root, firstBranchTip]);
		expect(secondBranch.entryIds).toEqual([root, secondBranchTip]);
		expect(firstBranch.messages).not.toContain(secondBranch.messages[1]);
	});

	it("uses stable entry ids and reconstructs every page without duplicates or gaps", () => {
		const manager = SessionManager.inMemory("/history");
		for (let index = 0; index < 1_005; index++) manager.appendMessage(user(`message-${index}`, index));
		const pinned = manager.buildSessionHistory();
		const initial = slicePinnedSessionHistory(pinned, {
			representation: "representation-1",
			generation: "generation-1",
			limit: 400,
		});
		expect(initial.messages).toHaveLength(MAX_HISTORY_RANGE_MESSAGES);
		expect(initial.startIndex).toBe(605);
		expect(initial.hasOlder).toBe(true);

		const pages = [initial];
		while (pages.at(-1)!.hasOlder) {
			pages.push(
				slicePinnedSessionHistory(pinned, {
					representation: "representation-1",
					generation: "generation-1",
					beforeEntryId: pages.at(-1)!.entryIds[0],
					limit: 400,
				}),
			);
		}
		const reconstructedMessages = pages
			.slice()
			.reverse()
			.flatMap((page) => page.messages);
		const reconstructedIds = pages
			.slice()
			.reverse()
			.flatMap((page) => page.entryIds);
		expect(reconstructedMessages).toEqual(pinned.messages);
		expect(reconstructedIds).toEqual(pinned.entryIds);
		expect(new Set(reconstructedIds).size).toBe(reconstructedIds.length);
	});

	it("pins the tip across concurrent appends and compaction", () => {
		const manager = SessionManager.inMemory("/history");
		const first = manager.appendMessage(user("first", 1));
		manager.appendMessage(assistant("answer", 2));
		manager.appendCompaction("summary", first, 100);
		const tip = manager.appendMessage(user("later", 4));
		const pinned = manager.buildSessionHistory(tip);
		expect(pinned.messages.map((message) => message.role)).toEqual([
			"user",
			"assistant",
			"compactionSummary",
			"user",
		]);
		const initial = slicePinnedSessionHistory(pinned, {
			representation: "representation-1",
			generation: "generation-1",
			limit: 2,
		});

		manager.appendMessage(assistant("concurrent append", 5));
		manager.appendCompaction("newer summary", tip, 200);
		const stillPinned = manager.buildSessionHistory(tip);
		const older = slicePinnedSessionHistory(stillPinned, {
			representation: "representation-1",
			generation: "generation-1",
			beforeEntryId: initial.entryIds[0],
			limit: 2,
		});
		expect([...older.messages, ...initial.messages]).toEqual(pinned.messages);
		expect([...older.entryIds, ...initial.entryIds]).toEqual(pinned.entryIds);
		expect(stillPinned.tipEntryId).toBe(tip);
	});

	it("rejects stale boundaries and invalid limits instead of returning partial history", () => {
		const manager = SessionManager.inMemory("/history");
		manager.appendMessage(user("one", 1));
		const pinned = manager.buildSessionHistory();
		expect(() =>
			slicePinnedSessionHistory(pinned, {
				representation: "representation-1",
				generation: "generation-1",
				beforeEntryId: "missing",
			}),
		).toThrow("boundary no longer exists");
		expect(() =>
			slicePinnedSessionHistory(pinned, {
				representation: "representation-1",
				generation: "generation-1",
				limit: 0,
			}),
		).toThrow("positive integer");
	});

	it("keeps full model context unchanged while history uses chronological presentation order", () => {
		const manager = SessionManager.inMemory("/history");
		const first = manager.appendMessage(user("first", 1));
		manager.appendMessage(assistant("answer", 2));
		manager.appendCompaction("summary", first, 100);
		manager.appendMessage(user("later", 4));
		const modelContext = manager.buildSessionContext();
		const history = manager.buildSessionHistory();
		expect(modelContext.messages.map((message) => message.role)).toEqual([
			"compactionSummary",
			"user",
			"assistant",
			"user",
		]);
		expect(history.messages.map((message) => message.role)).toEqual([
			"user",
			"assistant",
			"compactionSummary",
			"user",
		]);
	});

	it("preserves chronological ids through compaction and reopen", async () => {
		const directory = mkdtempSync(join(tmpdir(), "prime-session-history-reopen-"));
		try {
			const manager = SessionManager.create(directory, directory);
			manager.flushNow();
			const first = manager.appendMessage(user("first", 1));
			manager.appendMessage(assistant("answer", 2));
			manager.appendCompaction("summary", first, 100);
			manager.appendMessage(user("later", 4));
			const before = manager.buildSessionHistory();
			const reopened = await SessionManager.openAsync(manager.getSessionFile()!, directory);
			const after = reopened.buildSessionHistory();
			expect(after).toEqual(before);
			expect(after.messages.map((item) => item.role)).toEqual(["user", "assistant", "compactionSummary", "user"]);
		} finally {
			rmSync(directory, { recursive: true, force: true });
		}
	});

	it("keeps catalogue search text and counts beyond the visible initial window", async () => {
		const directory = mkdtempSync(join(tmpdir(), "prime-session-history-search-"));
		try {
			const manager = SessionManager.create(directory, directory);
			manager.flushNow();
			manager.appendMessage(user("catalogue-only-old-marker", 0));
			for (let index = 1; index < 450; index++) manager.appendMessage(user(`recent-${index}`, index));
			const initial = slicePinnedSessionHistory(manager.buildSessionHistory(), {
				representation: "representation-1",
				generation: "search",
			});
			expect(initial.messages).toHaveLength(400);
			expect(JSON.stringify(initial.messages)).not.toContain("catalogue-only-old-marker");

			const listed = await SessionManager.list(directory, directory);
			expect(listed).toHaveLength(1);
			expect(listed[0]?.messageCount).toBe(450);
			expect(listed[0]?.allMessagesText).toContain("catalogue-only-old-marker");
		} finally {
			rmSync(directory, { recursive: true, force: true });
		}
	});

	it.runIf(process.env.PRIME_AGENT_LARGE_HISTORY_FIXTURE !== undefined)(
		"reconstructs the explicit 30-80 MiB combined-validation fixture",
		async () => {
			const fixturePath = process.env.PRIME_AGENT_LARGE_HISTORY_FIXTURE!;
			const fixtureBytes = statSync(fixturePath).size;
			expect(fixtureBytes).toBeGreaterThanOrEqual(30 * 1024 * 1024);
			expect(fixtureBytes).toBeLessThanOrEqual(80 * 1024 * 1024);
			const manager = await SessionManager.openAsync(fixturePath);
			expect(manager.getLoadObservation()).toEqual({ readBytes: fixtureBytes });
			const pinned = manager.buildSessionHistory();
			let page = slicePinnedSessionHistory(pinned, {
				representation: "representation-1",
				generation: "large-generation",
			});
			const ids = [...page.entryIds];
			while (page.hasOlder) {
				page = slicePinnedSessionHistory(pinned, {
					representation: "representation-1",
					generation: "large-generation",
					beforeEntryId: page.entryIds[0],
				});
				ids.unshift(...page.entryIds);
			}
			expect(ids).toEqual(pinned.entryIds);
			expect(new Set(ids).size).toBe(ids.length);
		},
	);
});
