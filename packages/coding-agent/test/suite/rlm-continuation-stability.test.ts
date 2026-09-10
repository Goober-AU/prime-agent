import type { AgentMessage } from "@earendil-works/pi-agent-core";
import { type AssistantMessage, fauxAssistantMessage, fauxText, fauxThinking, type Usage } from "@earendil-works/pi-ai";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
	type AgentSessionMessageController,
	type AgentSessionMessageReceipt,
	createAgentSessionMessage,
} from "../../src/core/agent-messages.js";
import type { AgentSession } from "../../src/core/agent-session.js";
import type { HostRequestHandler } from "../../src/core/kernel/index.js";
import {
	boundedRlmVisibleText,
	classifyRlmChildTerminal,
	createRlmChildContinuationMessage,
	emptyRlmContinuationState,
	parseRlmContinuationState,
	RLM_CONTINUATION_STATE_CUSTOM_TYPE,
	type RlmContinuationState,
} from "../../src/core/rlm-continuation.js";
import { SessionManager } from "../../src/core/session-manager.js";
import { createHarness, getMessageText, type Harness, type HarnessOptions } from "./harness.js";

interface Internals {
	_performCompaction(options: { model: unknown; apiKey: string; signal: AbortSignal }): Promise<unknown>;
	_rlmContinuation: RlmContinuationState;
	_beginRlmParentTask(message: AgentMessage): void;
	_handleRlmChildTurnOutcome(message: AssistantMessage, queue?: boolean, reason?: string): unknown;
	_deliverPendingRlmResults(): Promise<void>;
	_restoreRlmContinuationState(): void;
	_runRlmReloadBackstop(): Promise<void>;
	_createKernelHostHandlers(): Record<string, HostRequestHandler>;
	_rlmExplicitRepliesInFlight: Set<Promise<AgentSessionMessageReceipt>>;
	_markExplicitRlmParentReply(ids: readonly string[]): void;
	_queuePendingRlmContinuation(): boolean;
}

function internals(session: AgentSession): Internals {
	return session as unknown as Internals;
}
function parentMessage(id: string) {
	return createAgentSessionMessage({
		id,
		source: "agent_message",
		message: `Task ${id}`,
		fromRelationship: "parent",
		from: { activeSessionId: "parent", sessionId: "parent-session" },
		target: { activeSessionId: "child", sessionId: "child-session" },
	});
}
function usage(total: number): Usage {
	return {
		input: total,
		output: 0,
		cacheRead: 0,
		cacheWrite: 0,
		totalTokens: total,
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
	};
}

describe("durable RLM continuation and terminal delivery", () => {
	const harnesses: Harness[] = [];
	afterEach(() => {
		vi.restoreAllMocks();
		while (harnesses.length) harnesses.pop()?.cleanup();
	});
	async function child(options: HarnessOptions = {}) {
		const reports: string[] = [];
		const send = vi.fn<AgentSessionMessageController["sendAgentMessage"]>(async (input) => {
			reports.push(input.message);
			return {
				id: `reply-${reports.length}`,
				source: "agent_message",
				target: { activeSessionId: "parent", sessionId: "parent-session" },
				message: input.message,
				deliveryStatus: "queued",
			};
		});
		const harness = await createHarness({
			rlmDepth: 1,
			persistSession: true,
			models: [{ id: "faux-1", contextWindow: 1_000_000 }],
			settings: { compaction: { enabled: false }, retry: { enabled: false } },
			agentMessageController: {
				listAgents: () => ({ agents: [] }),
				roster: () => ({
					current: { name: "child", id: "child", depth: 1 },
					entries: [{ relationship: "parent", name: "parent", id: "parent", depth: 0, status: "idle" }],
				}),
				sendAgentMessage: send,
			},
			...options,
		});
		harnesses.push(harness);
		return { ...harness, reports, send };
	}
	async function prompt(harness: Harness, id: string) {
		const message = parentMessage(id);
		await harness.session.prompt(message.content, { customMessage: message, expandPromptTemplates: false });
		await harness.session.waitForHeadlessIdle();
	}
	function compactLocally(): HarnessOptions["extensionFactories"] {
		return [
			(pi) => {
				pi.on("session_before_compact", (event) => ({
					compaction: {
						summary: "Local synthetic checkpoint",
						firstKeptEntryId: event.preparation.firstKeptEntryId,
						tokensBefore: event.preparation.tokensBefore,
						details: {},
					},
				}));
			},
		];
	}
	function seedHistory(harness: Harness): void {
		const history: AgentMessage[] = [
			{ role: "user", content: "Earlier completed work", timestamp: 1 },
			fauxAssistantMessage("Earlier result", { timestamp: 2 }),
		];
		for (const message of history) {
			if (message.role === "user" || message.role === "assistant") harness.sessionManager.appendMessage(message);
		}
		harness.session.agent.state.messages = history;
	}

	it.each(["length", "stop"] as const)(
		"preserves one %s recovery across the inclusive 250K threshold",
		async (stopReason) => {
			const harness = await child({
				settings: {
					compaction: {
						enabled: true,
						reserveTokens: 1000,
						keepRecentTokens: 1,
						summaryUpdatePolicy: "consolidate-repeated-v1",
					},
					retry: { enabled: false },
				},
				extensionFactories: compactLocally(),
			});
			seedHistory(harness);
			// Faux estimates usage from text; inject the provider's authoritative usage
			// sample at message completion without allocating a 1 MB fixture prompt.
			harness.session.agent.subscribe((event) => {
				if (
					event.type === "message_end" &&
					event.message.role === "assistant" &&
					getMessageText(event.message).startsWith("partial")
				)
					event.message.usage = usage(250_000);
			});
			harness.setResponses([
				fauxAssistantMessage(`partial ${"x".repeat(2000)}`, { stopReason }),
				(context) => {
					const text = context.messages.map(getMessageText).join("\n");
					expect(text).toContain(
						stopReason === "length" ? "RLM child length recovery 1/3" : "RLM child continuation 1/3",
					);
					return fauxAssistantMessage("bounded final report\nRLM_CHILD_STATUS: complete");
				},
			]);
			await prompt(harness, "initial");
			expect(harness.faux.state.callCount).toBe(2);
			expect(
				harness.eventsOfType("compaction_start").map((event) => event.reason),
				JSON.stringify(harness.eventsOfType("compaction_end")),
			).toEqual(["threshold"]);
			expect(harness.eventsOfType("compaction_end")[0]?.result).toBeDefined();
			expect(harness.reports).toHaveLength(1);
			expect(harness.reports[0]).toContain(`partial: ${stopReason === "length" ? "yes" : "no"}`);
			expect(harness.session.rlmDiagnostics?.terminalStatus).toBe("complete");
			expect(internals(harness.session)._rlmContinuation.continuationCount).toBe(1);
			const entries = harness.sessionManager.getEntries();
			const reservation = entries.findIndex(
				(entry) =>
					entry.type === "custom" &&
					entry.customType === RLM_CONTINUATION_STATE_CUSTOM_TYPE &&
					parseRlmContinuationState(entry.data)?.pendingContinuation?.phase === "reserved",
			);
			expect(reservation).toBeGreaterThan(0);
			expect(entries.findIndex((entry) => entry.type === "compaction")).toBeGreaterThan(reservation);
		},
	);

	it("does not continue a valid terminal marker at the threshold", async () => {
		const harness = await child({
			settings: {
				compaction: {
					enabled: true,
					reserveTokens: 1000,
					keepRecentTokens: 1,
					summaryUpdatePolicy: "consolidate-repeated-v1",
				},
			},
			extensionFactories: compactLocally(),
		});
		seedHistory(harness);
		harness.session.agent.subscribe((event) => {
			if (event.type === "message_end" && event.message.role === "assistant") event.message.usage = usage(250_000);
		});
		harness.setResponses([fauxAssistantMessage(`done ${"x".repeat(2000)}\nRLM_CHILD_STATUS: complete`)]);
		await prompt(harness, "terminal");
		expect(harness.faux.state.callCount).toBe(1);
		expect(harness.session.rlmDiagnostics?.continuationQueued).toBe(false);
		expect(harness.reports).toHaveLength(1);
	});

	it.each(["Compaction failed in fixture", "Compaction cancelled"])(
		"keeps one recovery reservation after %s",
		async (failure) => {
			const harness = await child({
				settings: {
					compaction: {
						enabled: true,
						reserveTokens: 1000,
						keepRecentTokens: 1,
						summaryUpdatePolicy: "consolidate-repeated-v1",
					},
					retry: { enabled: false },
				},
				extensionFactories: compactLocally(),
			});
			vi.spyOn(internals(harness.session), "_performCompaction").mockRejectedValueOnce(new Error(failure));
			seedHistory(harness);
			harness.session.agent.subscribe((event) => {
				if (
					event.type === "message_end" &&
					event.message.role === "assistant" &&
					getMessageText(event.message).startsWith("partial")
				)
					event.message.usage = usage(250_000);
			});
			harness.setResponses([
				fauxAssistantMessage("partial result", { stopReason: "length" }),
				fauxAssistantMessage("recovered report\nRLM_CHILD_STATUS: complete"),
			]);
			await prompt(harness, "compaction-failure");
			expect(harness.faux.state.callCount).toBe(2);
			expect(internals(harness.session)._rlmContinuation.continuationCount).toBe(1);
			expect(harness.reports).toHaveLength(1);
			expect(harness.session.rlmDiagnostics?.terminalStatus).toBe("complete");
		},
	);

	it("processes a parent input arriving during compaction once without replacing recovery", async () => {
		let arrived = false;
		let session: AgentSession;
		const later = parentMessage("later");
		const harness = await child({
			settings: {
				compaction: {
					enabled: true,
					reserveTokens: 1000,
					keepRecentTokens: 1,
					summaryUpdatePolicy: "consolidate-repeated-v1",
				},
				retry: { enabled: false },
			},
			extensionFactories: [
				(pi) => {
					pi.on("session_before_compact", async (event) => {
						if (!arrived) {
							arrived = true;
							await session.queueAgentMessagePrompt(later.content, "followUp", later);
						}
						return {
							compaction: {
								summary: "Checkpoint with queued parent input",
								firstKeptEntryId: event.preparation.firstKeptEntryId,
								tokensBefore: event.preparation.tokensBefore,
								details: {},
							},
						};
					});
				},
			],
		});
		session = harness.session;
		seedHistory(harness);
		harness.session.agent.subscribe((event) => {
			if (
				event.type === "message_end" &&
				event.message.role === "assistant" &&
				getMessageText(event.message).startsWith("partial")
			)
				event.message.usage = usage(250_000);
		});
		harness.setResponses([
			fauxAssistantMessage("partial result", { stopReason: "length" }),
			(context) => {
				const texts = context.messages.map(getMessageText).join("\n");
				expect(texts).toContain("RLM child length recovery 1/3");
				expect(texts).not.toContain("Task later");
				return fauxAssistantMessage("original report\nRLM_CHILD_STATUS: complete");
			},
			(context) => {
				expect(context.messages.map(getMessageText).join("\n")).toContain("Task later");
				return fauxAssistantMessage("later report\nRLM_CHILD_STATUS: complete");
			},
		]);
		await prompt(harness, "original");
		expect(harness.faux.state.callCount).toBe(3);
		expect(harness.reports).toHaveLength(2);
		expect(harness.reports[0]).toContain("task_id: original");
		expect(harness.reports[1]).toContain("task_id: later");
		expect(harness.session.rlmDiagnostics?.terminalStatus).toBe("complete");
	});

	it("returns one structured partial failure on the fourth incomplete ending", async () => {
		const harness = await child();
		harness.setResponses(
			[0, 1, 2, 3].map((index) =>
				fauxAssistantMessage(`progress ${index}`, { timestamp: index + 1, stopReason: "length" }),
			),
		);
		await prompt(harness, "exhaust");
		expect(harness.faux.state.callCount).toBe(4);
		expect(harness.reports).toHaveLength(1);
		expect(harness.reports[0]).toContain("terminal_status: failed");
		expect(harness.reports[0]).toContain("protocol_exhausted_after_3_continuations");
		expect(harness.session.rlmDiagnostics?.terminalStatus).toBe("failed");
	});

	it("automatically reports each initial and reused-child task without hidden reasoning", async () => {
		const harness = await child();
		harness.setResponses([
			fauxAssistantMessage([fauxThinking("private chain"), fauxText("first result\nRLM_CHILD_STATUS: complete")]),
			fauxAssistantMessage("second result\nRLM_CHILD_STATUS: blocked"),
		]);
		await prompt(harness, "initial");
		await prompt(harness, "follow-up");
		expect(harness.reports).toHaveLength(2);
		expect(harness.reports[0]).toContain("task_id: initial");
		expect(harness.reports[1]).toContain("task_id: follow-up");
		expect(harness.reports.join("\n")).not.toContain("private chain");
		expect(harness.session.rlmDiagnostics?.terminalStatus).toBe("blocked");
	});

	it("waits for a late explicit reply and suppresses an automatic duplicate", async () => {
		const harness = await child();
		const state = internals(harness.session);
		state._beginRlmParentTask(parentMessage("late"));
		state._handleRlmChildTurnOutcome(fauxAssistantMessage("result\nRLM_CHILD_STATUS: complete"));
		let resolveReply!: () => void;
		const explicit = new Promise<void>((resolve) => {
			resolveReply = resolve;
		}).then(async () => {
			const receipt = await harness.send({ target: "parent", message: "explicit result" });
			state._markExplicitRlmParentReply(["late"]);
			return receipt;
		});
		state._rlmExplicitRepliesInFlight.add(explicit);
		const automatic = state._deliverPendingRlmResults();
		await Promise.resolve();
		expect(harness.reports).toHaveLength(0);
		resolveReply();
		await automatic;
		state._rlmExplicitRepliesInFlight.delete(explicit);
		expect(harness.reports).toEqual(["explicit result"]);
	});

	it("does not relabel an earlier result as a newly delivered parent task", async () => {
		const harness = await child();
		const state = internals(harness.session);
		state._beginRlmParentTask(parentMessage("first"));
		state._handleRlmChildTurnOutcome(fauxAssistantMessage("first result\nRLM_CHILD_STATUS: complete"));
		const delivery = state._deliverPendingRlmResults();
		state._beginRlmParentTask(parentMessage("second"));
		await delivery;
		expect(harness.reports).toHaveLength(1);
		expect(harness.reports[0]).toContain("task_id: first");
		expect(state._rlmContinuation.tasks.find((task) => task.id === "second")?.replied).toBe(false);
	});

	it("retains an undelivered per-task result through reload and a later task", async () => {
		const harness = await child();
		const state = internals(harness.session);
		state._beginRlmParentTask(parentMessage("first"));
		state._handleRlmChildTurnOutcome(fauxAssistantMessage("first visible result\nRLM_CHILD_STATUS: blocked"));
		harness.send.mockRejectedValueOnce(new Error("parent temporarily unavailable"));
		await expect(state._deliverPendingRlmResults()).rejects.toThrow("parent temporarily unavailable");
		const path = harness.sessionManager.getSessionFile();
		if (!path) throw new Error("fixture has no persisted session");
		const saved = SessionManager.open(path)
			.getEntries()
			.reverse()
			.find((entry) => entry.type === "custom" && entry.customType === RLM_CONTINUATION_STATE_CUSTOM_TYPE);
		const restored = saved?.type === "custom" ? parseRlmContinuationState(saved.data) : undefined;
		if (!restored) throw new Error("fixture has no durable RLM state");
		state._rlmContinuation = restored;
		state._beginRlmParentTask(parentMessage("second"));
		state._handleRlmChildTurnOutcome(fauxAssistantMessage("second visible result\nRLM_CHILD_STATUS: complete"));
		await state._deliverPendingRlmResults();
		expect(harness.reports).toHaveLength(2);
		expect(harness.reports[0]).toContain("task_id: first");
		expect(harness.reports[0]).toContain("first visible result");
		expect(harness.reports[0]).not.toContain("second visible result");
		expect(harness.reports[1]).toContain("task_id: second");
		expect(harness.reports[1]).toContain("second visible result");
	});

	it("durably reserves once, keeps the attempt across abort, and resumes only on request", async () => {
		const harness = await child();
		const state = internals(harness.session);
		state._beginRlmParentTask(parentMessage("restart"));
		harness.session.requestAbort();
		const ending = fauxAssistantMessage("unfinished", { stopReason: "length" });
		state._handleRlmChildTurnOutcome(ending, true, "threshold");
		state._handleRlmChildTurnOutcome(ending, true, "threshold");
		expect(state._rlmContinuation.continuationCount).toBe(1);
		expect(harness.session.queuedActionCount).toBe(0);
		state._rlmContinuation = emptyRlmContinuationState();
		state._restoreRlmContinuationState();
		expect(state._rlmContinuation.pendingContinuation?.attempt).toBe(1);
		harness.setResponses([fauxAssistantMessage("recovered\nRLM_CHILD_STATUS: complete")]);
		harness.session.resumeQueuedWork();
		await harness.session.waitForHeadlessIdle();
		expect(harness.faux.state.callCount).toBe(1);
		expect(harness.reports).toHaveLength(1);
	});

	it("fails visibly instead of replaying a missing started recovery checkpoint", async () => {
		const harness = await child();
		const state = internals(harness.session);
		state._beginRlmParentTask(parentMessage("missing"));
		state._handleRlmChildTurnOutcome(fauxAssistantMessage("partial", { stopReason: "length" }));
		const pending = state._rlmContinuation.pendingContinuation;
		expect(pending).toBeDefined();
		if (pending) pending.phase = "started";
		await state._runRlmReloadBackstop();
		expect(harness.faux.state.callCount).toBe(0);
		expect(harness.reports).toHaveLength(1);
		expect(harness.reports[0]).toContain("recovery_checkpoint_missing");
	});

	it("does not replay a restored tool call whose result is missing", async () => {
		const harness = await child();
		const state = internals(harness.session);
		state._beginRlmParentTask(parentMessage("missing-tool"));
		state._handleRlmChildTurnOutcome(fauxAssistantMessage("partial", { stopReason: "length" }));
		const pending = state._rlmContinuation.pendingContinuation;
		if (!pending) throw new Error("fixture did not reserve recovery");
		pending.phase = "started";
		harness.session.agent.state.messages = [
			{ role: "user", content: pending.messageText, timestamp: pending.messageTimestamp },
			fauxAssistantMessage(
				{ type: "toolCall", id: "unfinished-tool", name: "ipython", arguments: { code: "mutating_work()" } },
				{ stopReason: "toolUse" },
			),
		];
		await state._runRlmReloadBackstop();
		expect(harness.faux.state.callCount).toBe(0);
		expect(harness.reports).toHaveLength(1);
		expect(harness.reports[0]).toContain("recovery_tool_result_missing");
		expect(harness.session.rlmDiagnostics?.terminalStatus).toBe("failed");
	});

	it("resumes after a restored tool call only when every result is present", async () => {
		const harness = await child();
		const state = internals(harness.session);
		state._beginRlmParentTask(parentMessage("completed-tool"));
		state._handleRlmChildTurnOutcome(fauxAssistantMessage("partial", { stopReason: "length" }));
		const pending = state._rlmContinuation.pendingContinuation;
		if (!pending) throw new Error("fixture did not reserve recovery");
		pending.phase = "started";
		harness.session.agent.state.messages = [
			{ role: "user", content: pending.messageText, timestamp: pending.messageTimestamp },
			fauxAssistantMessage(
				{ type: "toolCall", id: "completed-tool", name: "ipython", arguments: { code: "work()" } },
				{ stopReason: "toolUse" },
			),
			{
				role: "toolResult",
				toolCallId: "completed-tool",
				toolName: "ipython",
				content: [{ type: "text", text: "saved result" }],
				isError: false,
				timestamp: Date.now(),
			},
		];
		harness.setResponses([fauxAssistantMessage("recovered tool report\nRLM_CHILD_STATUS: complete")]);
		await state._runRlmReloadBackstop();
		await harness.session.waitForHeadlessIdle();
		expect(harness.faux.state.callCount).toBe(1);
		expect(harness.reports).toHaveLength(1);
		expect(harness.reports[0]).toContain("recovered tool report");
		expect(harness.session.rlmDiagnostics?.terminalStatus).toBe("complete");
	});
});

describe("RLM terminal protocol helpers", () => {
	it("requires an actual stop and a final visible marker", () => {
		for (const stopReason of ["length", "toolUse"] as const)
			expect(
				classifyRlmChildTerminal(fauxAssistantMessage("RLM_CHILD_STATUS: complete", { stopReason })).terminal,
			).toBe(false);
		expect(
			classifyRlmChildTerminal(
				fauxAssistantMessage([fauxThinking("RLM_CHILD_STATUS: complete"), fauxText("progress")]),
			).terminal,
		).toBe(false);
		expect(classifyRlmChildTerminal(fauxAssistantMessage("RLM_CHILD_STATUS: complete\nmore progress")).terminal).toBe(
			false,
		);
		expect(classifyRlmChildTerminal(fauxAssistantMessage("result\nRLM_CHILD_STATUS: blocked")).status).toBe(
			"blocked",
		);
	});
	it("bounds reports, retains report-only length recovery, and rejects corrupt persisted states", () => {
		expect(boundedRlmVisibleText("x".repeat(20_000)).length).toBeLessThanOrEqual(6000);
		expect(getMessageText(createRlmChildContinuationMessage(1, "length"))).toContain("Stop further research");
		expect(parseRlmContinuationState({ ...emptyRlmContinuationState(), continuationCount: 4 })).toBeUndefined();
		expect(
			parseRlmContinuationState({
				...emptyRlmContinuationState(),
				tasks: [{ id: "a", receivedAt: 1, replied: "yes" }],
			}),
		).toBeUndefined();
		expect(parseRlmContinuationState(emptyRlmContinuationState())).toEqual(emptyRlmContinuationState());
	});
});
