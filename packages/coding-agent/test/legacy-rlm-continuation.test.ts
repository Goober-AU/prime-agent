import type { AgentMessage } from "@earendil-works/pi-agent-core";
import type { AssistantMessage } from "@earendil-works/pi-ai";
import { describe, expect, test } from "vitest";
import { parseLegacyRlmContinuationState } from "../src/core/legacy-rlm-continuation.js";

const record = {
	version: 1,
	currentTask: { id: "task-1", receivedAt: 10 },
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
		taskId: "task-1",
		phase: "queued",
	},
};
const assistant: AssistantMessage = {
	role: "assistant",
	api: "openai-responses",
	provider: "openai",
	model: "test-model",
	content: [
		{ type: "thinking", thinking: "private reasoning" },
		{ type: "text", text: "partial" },
	],
	stopReason: "length",
	timestamp: 20,
	usage: {
		input: 1,
		output: 1,
		cacheRead: 0,
		cacheWrite: 0,
		totalTokens: 2,
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
	},
};

describe("legacy Windows RLM continuation ledger", () => {
	test.each(["reserved", "queued", "continuation_hook"])(
		"preserves an unstarted %s recovery without spending another attempt",
		(phase) => {
			const old = { ...record, pendingContinuation: { ...record.pendingContinuation, phase } };
			const copy = structuredClone(old);
			const state = parseLegacyRlmContinuationState(old, [assistant]);
			expect(state?.continuationCount).toBe(1);
			expect(state?.lastSourceKey).toBe(record.pendingContinuation.sourceKey);
			expect(state?.pendingContinuation?.phase).toBe(phase === "continuation_hook" ? "reserved" : phase);
			expect(state?.tasks).toEqual([{ id: "task-1", receivedAt: 10, replied: false }]);
			expect(old).toEqual(copy);
		},
	);
	test("correlates an already-persisted recovery so the ordinary backstop will not enqueue it twice", () => {
		const messages: AgentMessage[] = [
			assistant,
			{ role: "user", timestamp: 30, content: record.pendingContinuation.messageText },
		];
		expect(parseLegacyRlmContinuationState(record, messages)?.pendingContinuation?.phase).toBe("started");
		messages[1] = { role: "user", timestamp: 31, content: record.pendingContinuation.messageText };
		expect(parseLegacyRlmContinuationState(record, messages)?.pendingContinuation?.phase).toBe("queued");
	});
	test.each([true, false])("preserves explicit and runtime-delivered reply receipts (%s)", (replySent) => {
		const state = parseLegacyRlmContinuationState(
			{ ...record, replySent, deliveredTaskIds: replySent ? [] : ["task-1"], pendingContinuation: undefined },
			[assistant],
		);
		expect(state?.tasks[0].replied).toBe(true);
		expect(state?.pendingResult).toBeUndefined();
	});
	test("reports missing recovery as a bounded partial failure instead of rerunning unknown tools", () => {
		const state = parseLegacyRlmContinuationState({ ...record, pendingContinuation: undefined }, [assistant]);
		expect(state?.terminalStatus).toBe("failed");
		expect(state?.pendingResult).toMatchObject({ status: "failed", partial: true, text: "partial" });
		expect(JSON.stringify(state)).not.toContain("private reasoning");
		expect(state?.pendingContinuation).toBeUndefined();
	});
	test("retains valid terminal status and only this task's visible result", () => {
		const state = parseLegacyRlmContinuationState(
			{ ...record, pendingContinuation: undefined, terminalStatus: "blocked", lastStopReason: "stop" },
			[assistant],
		);
		expect(state?.pendingResult).toMatchObject({ status: "blocked", text: "partial", partial: true });
		const withoutThisTask = parseLegacyRlmContinuationState(
			{ ...record, pendingContinuation: undefined, currentTask: { id: "new-task", receivedAt: 21 } },
			[assistant],
		);
		expect(withoutThisTask?.pendingResult?.text).not.toBe("partial");
	});
	test.each([
		null,
		{},
		{ ...record, version: 2 },
		{ ...record, currentTask: undefined },
		{ ...record, continuationCount: 2 },
		{ ...record, deliveredTaskIds: [3] },
		{ ...record, pendingContinuation: { ...record.pendingContinuation, taskId: "other-task" } },
		{ ...record, pendingContinuation: { ...record.pendingContinuation, phase: "unknown" } },
	])("rejects malformed or uncorrelatable legacy state %#", (value) => {
		expect(parseLegacyRlmContinuationState(value, [])).toBeUndefined();
	});
});
