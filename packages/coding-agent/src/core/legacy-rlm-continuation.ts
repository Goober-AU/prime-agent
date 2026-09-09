import type { AgentMessage } from "@earendil-works/pi-agent-core";
import {
	boundedRlmVisibleText,
	parseRlmContinuationState,
	type RlmContinuationState,
	readRlmVisibleText,
} from "./rlm-continuation.js";

export const LEGACY_RLM_CONTINUATION_STATE_CUSTOM_TYPE = "prime-agent.rlm-continuation-state-v091";

function isRecord(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === "object" && !Array.isArray(value);
}

/** Read the pre-source-integration Windows ledger without rewriting saved sessions.
 * A missing execution checkpoint cannot safely authorize replay of old tool work.
 */
export function parseLegacyRlmContinuationState(
	value: unknown,
	messages: readonly AgentMessage[],
): RlmContinuationState | undefined {
	if (!isRecord(value) || value.version !== 1 || typeof value.replySent !== "boolean") return undefined;
	if (!Array.isArray(value.deliveredTaskIds) || !value.deliveredTaskIds.every((id) => typeof id === "string")) {
		return undefined;
	}
	const task = value.currentTask;
	if (
		task !== undefined &&
		(!isRecord(task) ||
			typeof task.id !== "string" ||
			task.id.length === 0 ||
			typeof task.receivedAt !== "number" ||
			!Number.isFinite(task.receivedAt))
	)
		return undefined;
	const pending = value.pendingContinuation;
	if (
		pending !== undefined &&
		(!isRecord(pending) || !["reserved", "queued", "continuation_hook"].includes(String(pending.phase)))
	) {
		return undefined;
	}
	// Without task identity, a pending recovery cannot be correlated or delivered.
	if (pending !== undefined && task === undefined) return undefined;
	if (isRecord(pending) && pending.taskId !== undefined && pending.taskId !== (task as Record<string, unknown>).id) {
		return undefined;
	}
	const started =
		isRecord(pending) &&
		messages.some((message) => {
			if (message.role !== "user" || message.timestamp !== pending.messageTimestamp) return false;
			const text =
				typeof message.content === "string"
					? message.content
					: message.content
							.filter((block) => block.type === "text")
							.map((block) => block.text)
							.join("\n");
			return text === pending.messageText;
		});
	const state = parseRlmContinuationState({
		version: 1,
		tasks:
			task === undefined
				? []
				: [
						{
							id: (task as Record<string, unknown>).id,
							receivedAt: (task as Record<string, unknown>).receivedAt,
							replied: value.replySent || value.deliveredTaskIds.includes((task as { id: string }).id),
						},
					],
		continuationCount: value.continuationCount,
		lastStopReason: value.lastStopReason,
		lastSourceKey: isRecord(pending) ? pending.sourceKey : undefined,
		terminalStatus: value.terminalStatus,
		compactionReason: value.compactionReason,
		taskHadLength: value.taskHadLength,
		pendingContinuation: isRecord(pending)
			? {
					sourceKey: pending.sourceKey,
					attempt: pending.attempt,
					previousStopReason: pending.previousStopReason,
					messageTimestamp: pending.messageTimestamp,
					messageText: pending.messageText,
					phase: started ? "started" : pending.phase === "queued" ? "queued" : "reserved",
				}
			: undefined,
	});
	if (!state) return undefined;
	if (state.pendingContinuation && state.pendingContinuation.attempt !== state.continuationCount) return undefined;
	const currentTask = state.tasks[0];
	if (!currentTask || currentTask.replied || state.pendingContinuation) return state;
	let visibleText = "";
	for (const message of [...messages].reverse()) {
		if (message.role === "assistant" && message.timestamp >= currentTask.receivedAt) {
			visibleText = readRlmVisibleText(message);
			break;
		}
	}
	const incomplete = !state.terminalStatus || state.lastStopReason === "length";
	const result = {
		status: incomplete ? ("failed" as const) : state.terminalStatus!,
		text: boundedRlmVisibleText(visibleText || "No visible result was saved for this legacy parent task."),
		partial: incomplete || state.taskHadLength,
		reason: incomplete
			? "Legacy session has no durable continuation checkpoint; parent direction is required before replaying work."
			: undefined,
		stopReason: state.lastStopReason,
	};
	state.terminalStatus = result.status;
	currentTask.result = result;
	state.pendingResult = result;
	return state;
}
