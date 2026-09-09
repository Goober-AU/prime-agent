import type { AgentMessage } from "@earendil-works/pi-agent-core";
import { fauxAssistantMessage } from "@earendil-works/pi-ai";
import { afterEach, describe, expect, it } from "vitest";
import { LEGACY_RLM_CONTINUATION_STATE_CUSTOM_TYPE } from "../../src/core/legacy-rlm-continuation.js";
import {
	emptyRlmContinuationState,
	RLM_CONTINUATION_STATE_CUSTOM_TYPE,
	type RlmContinuationState,
} from "../../src/core/rlm-continuation.js";
import { createHarness, getMessageText, type Harness } from "./harness.js";

const legacy = {
	version: 1,
	currentTask: { id: "legacy-parent-task", receivedAt: 10 },
	continuationCount: 1,
	lastStopReason: "length",
	taskHadLength: true,
	replySent: false,
	deliveredTaskIds: [],
	pendingContinuation: {
		sourceKey: "20:length:7:partial",
		attempt: 1,
		previousStopReason: "length",
		messageTimestamp: 30,
		messageText: "RLM child length recovery 1/3.",
		taskId: "legacy-parent-task",
		phase: "queued",
	},
};

function internals(harness: Harness) {
	return harness.session as unknown as {
		_rlmDepth: number;
		_rlmContinuation: RlmContinuationState;
		_restoreRlmContinuationState(): void;
		_runRlmReloadBackstop(): Promise<void>;
	};
}

describe("legacy RLM session loader", () => {
	const harnesses: Harness[] = [];
	afterEach(() => {
		while (harnesses.length) harnesses.pop()?.cleanup();
	});

	it("uses the newest eligible entry across both ledger names and rejects corrupt latest state", async () => {
		const harness = await createHarness();
		harnesses.push(harness);
		const state = internals(harness);
		state._rlmDepth = 1;
		harness.sessionManager.appendCustomEntry(LEGACY_RLM_CONTINUATION_STATE_CUSTOM_TYPE, legacy);
		state._restoreRlmContinuationState();
		expect(state._rlmContinuation.pendingContinuation?.attempt).toBe(1);
		const current = { ...emptyRlmContinuationState(), terminalStatus: "complete" };
		harness.sessionManager.appendCustomEntry(RLM_CONTINUATION_STATE_CUSTOM_TYPE, current);
		state._restoreRlmContinuationState();
		expect(state._rlmContinuation.terminalStatus).toBe("complete");
		harness.sessionManager.appendCustomEntry(LEGACY_RLM_CONTINUATION_STATE_CUSTOM_TYPE, legacy);
		state._restoreRlmContinuationState();
		expect(state._rlmContinuation.pendingContinuation?.attempt).toBe(1);
		harness.sessionManager.appendCustomEntry(LEGACY_RLM_CONTINUATION_STATE_CUSTOM_TYPE, { version: 100 });
		expect(() => state._restoreRlmContinuationState()).toThrow("invalid or uncorrelatable saved state");
	});

	it("reopens a saved started recovery without replaying or spending another attempt", async () => {
		const original = await createHarness({ persistSession: true });
		harnesses.push(original);
		const messages: AgentMessage[] = [
			{ role: "user", content: "original parent task", timestamp: 10 },
			fauxAssistantMessage("partial", { stopReason: "length", timestamp: 20 }),
			{ role: "user", content: legacy.pendingContinuation.messageText, timestamp: 30 },
			fauxAssistantMessage("saved final report\nRLM_CHILD_STATUS: complete", { timestamp: 40 }),
		];
		for (const message of messages) {
			if (message.role === "user" || message.role === "assistant") original.sessionManager.appendMessage(message);
		}
		original.sessionManager.appendCustomEntry(LEGACY_RLM_CONTINUATION_STATE_CUSTOM_TYPE, legacy);
		original.sessionManager.flushNow();
		const reports: string[] = [];
		const resumed = await createHarness({
			rlmDepth: 1,
			existingSessionFile: original.sessionManager.getSessionFile()!,
			agentMessageController: {
				listAgents: () => ({ agents: [] }),
				roster: () => ({
					current: { name: "child", id: "child", depth: 1 },
					entries: [{ relationship: "parent", name: "parent", id: "parent", depth: 0, status: "idle" }],
				}),
				sendAgentMessage: async (input) => {
					reports.push(input.message);
					return {
						id: "reply",
						source: "agent_message",
						target: { activeSessionId: "parent", sessionId: "parent-session" },
						message: input.message,
						deliveryStatus: "queued",
					};
				},
			},
		});
		harnesses.push(resumed);
		expect(internals(resumed)._rlmContinuation.pendingContinuation?.phase).toBe("started");
		await internals(resumed)._runRlmReloadBackstop();
		await resumed.session.waitForHeadlessIdle();
		expect(resumed.faux.state.callCount).toBe(0);
		expect(internals(resumed)._rlmContinuation.continuationCount).toBe(1);
		expect(
			resumed.session.messages.filter(
				(message) => message.role === "user" && getMessageText(message) === legacy.pendingContinuation.messageText,
			),
		).toHaveLength(1);
		expect(reports).toHaveLength(1);
		expect(reports[0]).toContain("saved final report");
		expect(reports[0]).toContain("terminal_status: complete");
		expect(
			resumed.sessionManager
				.getEntries()
				.find((entry) => entry.type === "custom" && entry.customType === LEGACY_RLM_CONTINUATION_STATE_CUSTOM_TYPE),
		).toMatchObject({ data: legacy });
	});
});
