import type { AgentMessage } from "@earendil-works/pi-agent-core";
import { describe, expect, it } from "vitest";
import type { AgentConnectionHistoryRange } from "../src/modes/agent-connection/types.js";
import {
	type LoadedAgentConnectionHistory,
	mergeOlderAgentConnectionHistory,
} from "../src/modes/interactive/interactive-mode.js";

const message = (text: string, timestamp: number): AgentMessage => ({ role: "user", content: text, timestamp });

function currentHistory(): LoadedAgentConnectionHistory {
	return {
		version: 1,
		generation: "generation-1",
		representation: "representation-1",
		tipEntryId: "tip-1",
		totalMessageCount: 4,
		startIndex: 2,
		messages: [message("third", 3), message("fourth", 4)],
		entryIds: ["entry-3", "entry-4"],
		hasOlder: true,
		order: "chronological",
	};
}

function olderRange(): AgentConnectionHistoryRange {
	return {
		version: 1,
		generation: "generation-1",
		representation: "representation-1",
		tipEntryId: "tip-1",
		totalMessageCount: 4,
		startIndex: 0,
		messages: [message("first", 1), message("second", 2)],
		entryIds: ["entry-1", "entry-2"],
		hasOlder: false,
		order: "chronological",
	};
}

describe("interactive recent-first history pagination", () => {
	it("prepends a contiguous chronological page without changing stable ids", () => {
		const merged = mergeOlderAgentConnectionHistory(currentHistory(), olderRange());
		expect(merged.messages.map((item) => (item.role === "user" ? item.content : undefined))).toEqual([
			"first",
			"second",
			"third",
			"fourth",
		]);
		expect(merged.entryIds).toEqual(["entry-1", "entry-2", "entry-3", "entry-4"]);
		expect(merged.startIndex).toBe(0);
		expect(merged.hasOlder).toBe(false);
	});

	it("rejects a page from another generation, tip or non-contiguous offset", () => {
		for (const range of [
			{ ...olderRange(), generation: "generation-2" },
			{ ...olderRange(), representation: "representation-2" },
			{ ...olderRange(), tipEntryId: "tip-2" },
			{ ...olderRange(), startIndex: 1 },
			{ ...olderRange(), hasOlder: true },
			{ ...olderRange(), order: "newest-first" as never },
		]) {
			expect(() => mergeOlderAgentConnectionHistory(currentHistory(), range)).toThrow(
				"does not continue the pinned snapshot",
			);
		}
	});

	it("rejects duplicate ids and message/id misalignment", () => {
		expect(() =>
			mergeOlderAgentConnectionHistory(currentHistory(), {
				...olderRange(),
				entryIds: ["entry-3", "entry-2"],
			}),
		).toThrow("overlaps already loaded messages");
		expect(() =>
			mergeOlderAgentConnectionHistory(currentHistory(), { ...olderRange(), entryIds: ["entry-1"] }),
		).toThrow("does not continue the pinned snapshot");
		expect(() =>
			mergeOlderAgentConnectionHistory(currentHistory(), {
				...olderRange(),
				entryIds: ["entry-1", "entry-1"],
			}),
		).toThrow("overlaps already loaded messages");
	});
});
