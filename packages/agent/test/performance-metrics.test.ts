import {
	type AssistantMessage,
	type Context,
	createAssistantMessageEventStream,
	type Model,
	type SimpleStreamOptions,
} from "@earendil-works/pi-ai";
import { Type } from "typebox";
import { describe, expect, it } from "vitest";
import { finalizePerformanceMetricLogicalRequest, runAgentLoop } from "../src/agent-loop.js";
import {
	elapsedMetricMs,
	type PerformanceMetricEvent,
	type PerformanceMetricRecorder,
	performanceMetricUsageFromAssistant,
} from "../src/performance-metrics.js";
import type { AgentContext, AgentLoopConfig, AgentMessage, AgentTool, StreamFn } from "../src/types.js";

const model: Model<"openai-responses"> = {
	id: "gpt-test",
	name: "gpt-test",
	api: "openai-responses",
	provider: "openai",
	baseUrl: "https://example.invalid",
	reasoning: true,
	input: ["text"],
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	contextWindow: 8192,
	maxTokens: 1024,
};

function usage(input = 0, cacheRead = 0, output = 0) {
	return {
		input,
		output,
		cacheRead,
		cacheWrite: 0,
		totalTokens: input + cacheRead + output,
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
	};
}

function assistant(
	content: AssistantMessage["content"],
	stopReason: AssistantMessage["stopReason"] = "stop",
	messageUsage = usage(),
): AssistantMessage {
	return {
		role: "assistant",
		content,
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: messageUsage,
		stopReason,
		timestamp: 1,
		...(stopReason === "error" ? { errorMessage: "PRIVATE_PROVIDER_ERROR" } : {}),
	};
}

class FakeRecorder implements PerformanceMetricRecorder {
	readonly sessionId = "session-metrics";
	readonly events: PerformanceMetricEvent[] = [];
	private now = 0;
	private id = 0;
	throwOnRecord = false;

	monotonicNow(): number {
		this.now += 5;
		return this.now;
	}

	nextId(scope: "logical_request" | "provider_attempt"): string {
		return `${scope}-${++this.id}`;
	}

	record(event: PerformanceMetricEvent): void {
		if (this.throwOnRecord) throw new Error("synthetic recorder failure");
		this.events.push(event);
	}

	async flush(): Promise<void> {}
	async close(): Promise<void> {}
}

function baseContext(): AgentContext {
	return {
		systemPrompt: "system",
		messages: [],
		tools: [],
	};
}

function baseConfig(recorder?: FakeRecorder): AgentLoopConfig {
	return {
		model,
		convertToLlm: (messages) => messages as Context["messages"],
		...(recorder ? { performanceMetrics: { recorder } } : {}),
	};
}

function completedStream(
	message: AssistantMessage,
	options: SimpleStreamOptions | undefined,
	onPayload: (serialized: string) => void = () => undefined,
): ReturnType<typeof createAssistantMessageEventStream> {
	const stream = createAssistantMessageEventStream();
	queueMicrotask(() => {
		void (async () => {
			const originalPayload = { model: "gpt-test", input: [{ role: "user", content: "fixture" }] };
			const replacement = await options?.onPayload?.(originalPayload, model);
			onPayload(JSON.stringify(replacement ?? originalPayload));
			await options?.onResponse?.({ status: 200, headers: {} }, model);
			const partial = { ...message, content: [] };
			stream.push({ type: "start", partial });
			for (let index = 0; index < message.content.length; index++) {
				const block = message.content[index];
				if (block.type === "text") {
					stream.push({ type: "text_start", contentIndex: index, partial: message });
					stream.push({ type: "text_delta", contentIndex: index, delta: block.text, partial: message });
					stream.push({ type: "text_end", contentIndex: index, content: block.text, partial: message });
				} else if (block.type === "thinking") {
					stream.push({ type: "thinking_start", contentIndex: index, partial: message });
					stream.push({ type: "thinking_delta", contentIndex: index, delta: block.thinking, partial: message });
					stream.push({ type: "thinking_end", contentIndex: index, content: block.thinking, partial: message });
				}
			}
			if (message.stopReason === "error") {
				stream.push({ type: "error", reason: "error", error: message });
			} else if (message.stopReason === "aborted") {
				stream.push({ type: "error", reason: "aborted", error: message });
			} else {
				stream.push({ type: "done", reason: message.stopReason, message });
			}
		})();
	});
	return stream;
}

async function runSingle(
	message: AssistantMessage,
	config: AgentLoopConfig,
	onProviderOptions?: (options: SimpleStreamOptions | undefined) => void,
	onSerializedPayload?: (payload: string) => void,
): Promise<AgentMessage[]> {
	const streamFn: StreamFn = (_model, _context, options) => {
		onProviderOptions?.(options);
		return completedStream(message, options, onSerializedPayload);
	};
	return runAgentLoop(
		[{ role: "user", content: "fixture", timestamp: 1 }],
		baseContext(),
		config,
		() => undefined,
		undefined,
		streamFn,
	);
}

describe("performance metric helpers", () => {
	it("uses monotonic deltas and rejects unavailable or backwards clocks", () => {
		expect(elapsedMetricMs(10, 12.5)).toBe(2.5);
		expect(elapsedMetricMs(undefined, 12)).toBeNull();
		expect(elapsedMetricMs(12, 10)).toBeNull();
	});

	it("does not manufacture provider usage from all-zero placeholders", () => {
		expect(performanceMetricUsageFromAssistant(assistant([]))).toMatchObject({
			inputTokens: null,
			cachedInputTokens: null,
			outputTokens: null,
			reasoningTokens: null,
			totalTokens: null,
			cachedInputIncludedInInput: null,
		});
	});

	it("does not promote per-field normalized zero placeholders when another field is positive", () => {
		expect(performanceMetricUsageFromAssistant(assistant([], "stop", usage(100, 0, 0)))).toMatchObject({
			inputTokens: 100,
			cachedInputTokens: null,
			outputTokens: null,
			reasoningTokens: null,
			totalTokens: 100,
			cachedInputIncludedInInput: null,
			reasoningIncludedInOutput: null,
		});
	});
});

describe("agent-loop performance metrics", () => {
	it("records success and preserves the exact serialized provider payload", async () => {
		const withoutMetricsPayloads: string[] = [];
		const withMetricsPayloads: string[] = [];
		const replacementPayload = { stable: true, nested: { value: 1 } };
		const noMetricsConfig = baseConfig();
		noMetricsConfig.onPayload = () => replacementPayload;
		await runSingle(
			assistant([{ type: "text", text: "ok" }], "stop", usage(100, 20, 10)),
			noMetricsConfig,
			undefined,
			(payload) => withoutMetricsPayloads.push(payload),
		);

		const recorder = new FakeRecorder();
		const metricsConfig = baseConfig(recorder);
		metricsConfig.onPayload = () => replacementPayload;
		await runSingle(
			assistant([{ type: "text", text: "ok" }], "stop", usage(100, 20, 10)),
			metricsConfig,
			(options) => expect("performanceMetrics" in (options ?? {})).toBe(false),
			(payload) => withMetricsPayloads.push(payload),
		);

		expect(withMetricsPayloads).toEqual(withoutMetricsPayloads);
		expect(withMetricsPayloads).toEqual([JSON.stringify(replacementPayload)]);
		expect(recorder.events.map((event) => event.operation)).toEqual(["provider_attempt", "logical_request"]);
		expect(recorder.events[0]).toMatchObject({
			outcome: "success",
			usage: {
				inputTokens: 100,
				cachedInputTokens: 20,
				outputTokens: 10,
				reasoningTokens: null,
				cachedInputIncludedInInput: null,
			},
		});
		expect(recorder.events[1].usage).toBeUndefined();
		expect(recorder.events[0].measurements?.attempt_ordinal).toBe(1);
		expect(recorder.events[1].measurements?.attempt_count).toBeNull();
		expect(JSON.stringify(recorder.events)).not.toContain("fixture");
	});

	it("defers one idempotent logical terminal to a retry-owning host", async () => {
		const recorder = new FakeRecorder();
		const config = baseConfig(recorder);
		config.performanceMetrics = {
			recorder,
			logicalRequestId: "logical-host-owned",
			logicalRequestStartedAt: recorder.monotonicNow(),
			providerAttemptNumber: 2,
			hostOwnsLogicalRequestTerminal: true,
		};
		const messages = await runSingle(assistant([{ type: "text", text: "ok" }], "stop", usage(7, 0, 2)), config);
		expect(recorder.events.map((event) => event.operation)).toEqual(["provider_attempt"]);
		const terminal = messages.find((message): message is AssistantMessage => message.role === "assistant");
		expect(terminal).toBeDefined();
		finalizePerformanceMetricLogicalRequest(terminal as AssistantMessage, "success");
		finalizePerformanceMetricLogicalRequest(terminal as AssistantMessage, "failure");
		expect(recorder.events.map((event) => event.operation)).toEqual(["provider_attempt", "logical_request"]);
		expect(recorder.events[0].usage?.inputTokens).toBe(7);
		expect(recorder.events[1]).toMatchObject({
			correlation: { logicalRequestId: "logical-host-owned" },
			outcome: "success",
		});
		expect(recorder.events[1].correlation?.providerAttemptId).toBeUndefined();
		expect(recorder.events[1].usage).toBeUndefined();
	});

	it("settles host-owned pre-message failures instead of leaking a logical terminal", async () => {
		const recorder = new FakeRecorder();
		const config = baseConfig(recorder);
		config.performanceMetrics = { recorder, hostOwnsLogicalRequestTerminal: true };
		const streamFn: StreamFn = () => {
			throw new Error("pre-message failure");
		};
		await expect(
			runAgentLoop(
				[{ role: "user", content: "fixture", timestamp: 1 }],
				baseContext(),
				config,
				() => undefined,
				undefined,
				streamFn,
			),
		).rejects.toThrow("pre-message failure");
		expect(recorder.events.map((event) => [event.operation, event.outcome])).toEqual([
			["provider_attempt", "failure"],
			["logical_request", "failure"],
		]);
	});

	it("keeps absent usage and reasoning-only visible latency unavailable", async () => {
		const recorder = new FakeRecorder();
		await runSingle(assistant([{ type: "thinking", thinking: "PRIVATE_REASONING" }]), baseConfig(recorder));
		const request = recorder.events.find((event) => event.operation === "logical_request");
		const attempt = recorder.events.find((event) => event.operation === "provider_attempt");
		expect(request?.measurements?.dispatch_to_first_visible_ms).toBeNull();
		expect(request?.usage).toBeUndefined();
		expect(attempt?.usage?.inputTokens).toBeNull();
		expect(JSON.stringify(request)).not.toContain("PRIVATE_REASONING");
	});

	it("contains usage-observer errors even when metric persistence is off", async () => {
		const config = baseConfig();
		config.onUsageObservation = async () => {
			throw new Error("disposable observer without recorder");
		};
		const streamFn: StreamFn = (_model, _context, options) => {
			const stream = createAssistantMessageEventStream();
			queueMicrotask(() => {
				options?.onUsageObservation?.(
					{
						inputTokens: 1,
						cachedInputTokens: null,
						outputTokens: 1,
						reasoningTokens: null,
						totalTokens: 2,
						cachedInputIncludedInInput: true,
						reasoningIncludedInOutput: true,
					},
					model,
				);
				const message = assistant([{ type: "text", text: "still completes without metrics" }]);
				stream.push({ type: "start", partial: { ...message, content: [] } });
				stream.push({ type: "done", reason: "stop", message });
			});
			return stream;
		};
		await expect(
			runAgentLoop(
				[{ role: "user", content: "fixture", timestamp: 1 }],
				baseContext(),
				config,
				() => undefined,
				undefined,
				streamFn,
			),
		).resolves.toEqual(expect.arrayContaining([expect.objectContaining({ role: "assistant" })]));
	});

	it("prefers authoritative raw usage including explicit zeros and contains caller observer errors", async () => {
		const recorder = new FakeRecorder();
		const config = baseConfig(recorder);
		config.onUsageObservation = () => {
			throw new Error("disposable caller observer");
		};
		const streamFn: StreamFn = (_model, _context, options) => {
			const stream = createAssistantMessageEventStream();
			queueMicrotask(() => {
				options?.onUsageObservation?.(
					{
						inputTokens: 0,
						cachedInputTokens: null,
						outputTokens: 0,
						reasoningTokens: 0,
						totalTokens: 0,
						cachedInputIncludedInInput: true,
						reasoningIncludedInOutput: true,
					},
					model,
				);
				const message = assistant([{ type: "text", text: "still completes" }]);
				stream.push({ type: "start", partial: { ...message, content: [] } });
				stream.push({ type: "done", reason: "stop", message });
			});
			return stream;
		};
		await runAgentLoop(
			[{ role: "user", content: "fixture", timestamp: 1 }],
			baseContext(),
			config,
			() => undefined,
			undefined,
			streamFn,
		);
		const request = recorder.events.find((event) => event.operation === "provider_attempt");
		expect(request?.usage).toEqual({
			source: "provider",
			inputTokens: 0,
			cachedInputTokens: null,
			outputTokens: 0,
			reasoningTokens: 0,
			totalTokens: 0,
			cachedInputIncludedInInput: true,
			reasoningIncludedInOutput: true,
		});
	});

	it("records failure and cancellation without persisting error text", async () => {
		const failedRecorder = new FakeRecorder();
		await runSingle(assistant([], "error"), baseConfig(failedRecorder));
		expect(failedRecorder.events[0].outcome).toBe("failure");
		expect(JSON.stringify(failedRecorder.events)).not.toContain("PRIVATE_PROVIDER_ERROR");

		const cancelledRecorder = new FakeRecorder();
		const controller = new AbortController();
		const streamFn: StreamFn = () => {
			const stream = createAssistantMessageEventStream();
			queueMicrotask(() => controller.abort());
			return stream;
		};
		await runAgentLoop(
			[{ role: "user", content: "fixture", timestamp: 1 }],
			baseContext(),
			baseConfig(cancelledRecorder),
			() => undefined,
			controller.signal,
			streamFn,
		);
		expect(cancelledRecorder.events[0].outcome).toBe("cancelled");
	});

	it("correlates separately supplied host retry ordinals without claiming an upstream count", async () => {
		const recorder = new FakeRecorder();
		for (const providerAttemptNumber of [1, 2]) {
			const config = baseConfig(recorder);
			config.performanceMetrics = {
				recorder,
				logicalRequestId: "logical-retry",
				providerAttemptNumber,
			};
			await runSingle(assistant([{ type: "text", text: "result" }]), config);
		}
		const attempts = recorder.events.filter((event) => event.operation === "provider_attempt");
		expect(attempts.map((event) => event.correlation?.logicalRequestId)).toEqual(["logical-retry", "logical-retry"]);
		expect(attempts.map((event) => event.measurements?.attempt_ordinal)).toEqual([1, 2]);
		expect(attempts.every((event) => event.measurements?.attempt_count === null)).toBe(true);
	});

	it("records one terminal tool duration and contains recorder failures", async () => {
		const recorder = new FakeRecorder();
		const tool: AgentTool = {
			name: "private-tool-name",
			label: "Private tool label",
			description: "Private tool fixture",
			parameters: Type.Object({}),
			execute: async () => ({ content: [{ type: "text", text: "PRIVATE_TOOL_OUTPUT" }], details: {} }),
		};
		const context = baseContext();
		context.tools = [tool];
		let call = 0;
		const streamFn: StreamFn = (_model, _context, options) => {
			call++;
			return completedStream(
				call === 1
					? assistant([{ type: "toolCall", id: "tool-1", name: tool.name, arguments: {} }], "toolUse")
					: assistant([{ type: "text", text: "done" }]),
				options,
			);
		};
		await runAgentLoop(
			[{ role: "user", content: "fixture", timestamp: 1 }],
			context,
			baseConfig(recorder),
			() => undefined,
			undefined,
			streamFn,
		);
		const attempts = recorder.events.filter((event) => event.operation === "provider_attempt");
		const logicalRequests = recorder.events.filter((event) => event.operation === "logical_request");
		expect(attempts).toHaveLength(2);
		expect(logicalRequests).toHaveLength(2);
		expect(new Set(logicalRequests.map((event) => event.correlation?.logicalRequestId)).size).toBe(2);
		expect(logicalRequests.every((event) => event.usage === undefined)).toBe(true);
		const toolMetric = recorder.events.find((event) => event.operation === "tool");
		expect(toolMetric).toMatchObject({
			correlation: { toolCallId: "tool-1" },
			outcome: "success",
		});
		expect(toolMetric?.measurements?.total_ms).toBeGreaterThanOrEqual(0);
		expect(JSON.stringify(toolMetric)).not.toContain("PRIVATE_TOOL_OUTPUT");
		expect(JSON.stringify(toolMetric)).not.toContain("private-tool-name");

		const throwingRecorder = new FakeRecorder();
		throwingRecorder.throwOnRecord = true;
		await expect(
			runSingle(assistant([{ type: "text", text: "still succeeds" }]), baseConfig(throwingRecorder)),
		).resolves.toEqual(
			expect.arrayContaining([
				expect.objectContaining({
					role: "assistant",
					stopReason: "stop",
					content: [{ type: "text", text: "still succeeds" }],
				}),
			]),
		);
	});
});
