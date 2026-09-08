import type { AssistantMessage, StopReason, UserMessage } from "@earendil-works/pi-ai";

export const RLM_CHILD_MAX_CONTINUATIONS = 3;
export const RLM_CONTINUATION_STATE_CUSTOM_TYPE = "prime-agent.rlm-continuation-state";
export type RlmTerminalStatus = "complete" | "blocked" | "failed";

export interface RlmParentTask {
	id: string;
	receivedAt: number;
	replied: boolean;
	result?: RlmPendingResult;
}

export interface RlmPendingContinuation {
	sourceKey: string;
	attempt: number;
	previousStopReason: StopReason;
	messageTimestamp: number;
	messageText: string;
	phase: "reserved" | "queued" | "started";
}

export interface RlmPendingResult {
	status: RlmTerminalStatus;
	text: string;
	partial: boolean;
	reason?: string;
	stopReason?: StopReason;
}

export interface RlmContinuationState {
	version: 1;
	tasks: RlmParentTask[];
	continuationCount: number;
	lastStopReason?: StopReason;
	lastSourceKey?: string;
	terminalStatus?: RlmTerminalStatus;
	compactionReason?: string;
	taskHadLength: boolean;
	pendingContinuation?: RlmPendingContinuation;
	pendingResult?: RlmPendingResult;
}

export function emptyRlmContinuationState(): RlmContinuationState {
	return { version: 1, tasks: [], continuationCount: 0, taskHadLength: false };
}

export function readRlmVisibleText(message: AssistantMessage): string {
	return message.content
		.filter((block) => block.type === "text")
		.map((block) => block.text)
		.join("")
		.trim();
}

export function classifyRlmChildTerminal(message: AssistantMessage): {
	terminal: boolean;
	status?: RlmTerminalStatus;
	text: string;
	canContinue: boolean;
} {
	const text = readRlmVisibleText(message);
	// An output-limited response cannot certify completion, even if it happened
	// to emit a terminal marker before the provider cut it off.
	if (message.stopReason === "error" || message.stopReason === "aborted") {
		return { terminal: true, status: "failed", text, canContinue: false };
	}
	const match =
		message.stopReason === "stop" && text.match(/(?:^|\n)RLM_CHILD_STATUS:\s*(complete|blocked|failed)\s*$/i);
	if (match) return { terminal: true, status: match[1].toLowerCase() as RlmTerminalStatus, text, canContinue: false };
	return { terminal: false, text, canContinue: message.stopReason === "stop" || message.stopReason === "length" };
}

export function createRlmChildContinuationMessage(
	attempt: number,
	previousStopReason: StopReason,
	timestamp = Date.now(),
): UserMessage {
	const text =
		previousStopReason === "length"
			? [
					`RLM child length recovery ${attempt}/${RLM_CHILD_MAX_CONTINUATIONS}.`,
					"Your previous response reached its output limit. Stop further research, exploration, file reads, code changes, background work, and ordinary tool use now.",
					"Immediately send the parent one concise partial-result report with agent_message.send, then end with exactly one terminal line: RLM_CHILD_STATUS: complete, RLM_CHILD_STATUS: blocked, or RLM_CHILD_STATUS: failed.",
				]
			: [
					`RLM child continuation ${attempt}/${RLM_CHILD_MAX_CONTINUATIONS}.`,
					"Your previous response ended without the required terminal status. Continue the same task without repeating completed work.",
					"Before ending, send the substantive result or blocker to the parent when appropriate, then end with exactly one terminal line: RLM_CHILD_STATUS: complete, RLM_CHILD_STATUS: blocked, or RLM_CHILD_STATUS: failed.",
				];
	return { role: "user", content: [{ type: "text", text: text.join("\n") }], timestamp };
}

export function boundedRlmVisibleText(text: string, maxChars = 6000): string {
	const normalized = text.trim();
	return normalized.length <= maxChars
		? normalized
		: `${normalized.slice(0, maxChars - 32).trimEnd()}\n[truncated by Prime runtime]`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isStopReason(value: unknown): value is StopReason {
	return value === "stop" || value === "length" || value === "toolUse" || value === "error" || value === "aborted";
}

function isTerminalStatus(value: unknown): value is RlmTerminalStatus {
	return value === "complete" || value === "blocked" || value === "failed";
}

function isPendingResult(value: unknown): value is RlmPendingResult {
	return (
		isRecord(value) &&
		isTerminalStatus(value.status) &&
		typeof value.text === "string" &&
		typeof value.partial === "boolean" &&
		(value.reason === undefined || typeof value.reason === "string") &&
		(value.stopReason === undefined || isStopReason(value.stopReason))
	);
}

export function parseRlmContinuationState(value: unknown): RlmContinuationState | undefined {
	if (
		!isRecord(value) ||
		value.version !== 1 ||
		!Array.isArray(value.tasks) ||
		!Number.isSafeInteger(value.continuationCount) ||
		(value.continuationCount as number) < 0 ||
		(value.continuationCount as number) > RLM_CHILD_MAX_CONTINUATIONS ||
		typeof value.taskHadLength !== "boolean"
	)
		return undefined;
	if (
		!value.tasks.every(
			(task) =>
				isRecord(task) &&
				typeof task.id === "string" &&
				typeof task.receivedAt === "number" &&
				Number.isFinite(task.receivedAt) &&
				typeof task.replied === "boolean" &&
				(task.result === undefined || isPendingResult(task.result)),
		)
	)
		return undefined;
	if (value.lastStopReason !== undefined && !isStopReason(value.lastStopReason)) return undefined;
	if (value.lastSourceKey !== undefined && typeof value.lastSourceKey !== "string") return undefined;
	if (value.terminalStatus !== undefined && !isTerminalStatus(value.terminalStatus)) return undefined;
	if (value.compactionReason !== undefined && typeof value.compactionReason !== "string") return undefined;
	const pending = value.pendingContinuation;
	if (
		pending !== undefined &&
		(!isRecord(pending) ||
			typeof pending.sourceKey !== "string" ||
			!Number.isSafeInteger(pending.attempt) ||
			(pending.attempt as number) < 1 ||
			(pending.attempt as number) > RLM_CHILD_MAX_CONTINUATIONS ||
			!isStopReason(pending.previousStopReason) ||
			typeof pending.messageTimestamp !== "number" ||
			!Number.isFinite(pending.messageTimestamp) ||
			typeof pending.messageText !== "string" ||
			!["reserved", "queued", "started"].includes(String(pending.phase)))
	)
		return undefined;
	const result = value.pendingResult;
	if (result !== undefined && !isPendingResult(result)) return undefined;
	return structuredClone(value) as unknown as RlmContinuationState;
}
