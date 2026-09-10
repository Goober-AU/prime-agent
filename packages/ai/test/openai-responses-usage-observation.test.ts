import type { ResponseStreamEvent } from "openai/resources/responses/responses.js";
import { afterEach, describe, expect, it, vi } from "vitest";
import { getModel } from "../src/models.js";
import { streamAzureOpenAIResponses } from "../src/providers/azure-openai-responses.js";
import { streamOpenAICodexResponses } from "../src/providers/openai-codex-responses.js";
import { streamOpenAIResponses } from "../src/providers/openai-responses.js";
import { processResponsesStream } from "../src/providers/openai-responses-shared.js";
import type { AssistantMessage, Context, Model, ProviderUsageObservation } from "../src/types.js";
import { AssistantMessageEventStream } from "../src/utils/event-stream.js";

const context: Context = {
	systemPrompt: "system",
	messages: [{ role: "user", content: "hello", timestamp: 1 }],
};

function outputFor(model: Model<any>): AssistantMessage {
	return {
		role: "assistant",
		content: [],
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "stop",
		timestamp: 1,
	};
}

async function* completedEvents(usage: Record<string, unknown>): AsyncIterable<ResponseStreamEvent> {
	yield {
		type: "response.completed",
		response: { id: "resp_usage", status: "completed", usage },
	} as unknown as ResponseStreamEvent;
}

function sse(usage: Record<string, unknown>): Response {
	const events = [
		{ type: "response.created", response: { id: "resp_usage", status: "in_progress" } },
		{ type: "response.completed", response: { id: "resp_usage", status: "completed", usage } },
	];
	return new Response(`${events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("")}data: [DONE]\n\n`, {
		status: 200,
		headers: { "content-type": "text/event-stream" },
	});
}

afterEach(() => {
	vi.restoreAllMocks();
});

describe("Responses raw usage observation", () => {
	it("preserves authoritative raw zeros, inclusive overlap flags, and absent fields", async () => {
		const model = getModel("openai", "gpt-5.4");
		const zeroObservation = vi.fn<(observation: ProviderUsageObservation) => void>();
		await processResponsesStream(
			completedEvents({
				input_tokens: 0,
				input_tokens_details: { cached_tokens: 0 },
				output_tokens: 0,
				output_tokens_details: { reasoning_tokens: 0 },
				total_tokens: 0,
			}),
			outputFor(model),
			new AssistantMessageEventStream(),
			model,
			{ onUsageObservation: zeroObservation },
		);
		expect(zeroObservation).toHaveBeenCalledWith(
			{
				inputTokens: 0,
				cachedInputTokens: 0,
				outputTokens: 0,
				reasoningTokens: 0,
				totalTokens: 0,
				cachedInputIncludedInInput: true,
				reasoningIncludedInOutput: true,
			},
			model,
		);

		const absentObservation = vi.fn<(observation: ProviderUsageObservation) => void>();
		await processResponsesStream(
			completedEvents({ input_tokens: 9, output_tokens: 4 }),
			outputFor(model),
			new AssistantMessageEventStream(),
			model,
			{ onUsageObservation: absentObservation },
		);
		expect(absentObservation.mock.calls[0]?.[0]).toEqual({
			inputTokens: 9,
			cachedInputTokens: null,
			outputTokens: 4,
			reasoningTokens: null,
			totalTokens: null,
			cachedInputIncludedInInput: true,
			reasoningIncludedInOutput: true,
		});
	});

	it("reports reasoning tokens and contains observer failures without changing output", async () => {
		const model = getModel("openai", "gpt-5.4");
		const output = outputFor(model);
		await expect(
			processResponsesStream(
				completedEvents({
					input_tokens: 12,
					input_tokens_details: { cached_tokens: 5 },
					output_tokens: 8,
					output_tokens_details: { reasoning_tokens: 7 },
					total_tokens: 20,
				}),
				output,
				new AssistantMessageEventStream(),
				model,
				{
					onUsageObservation: async () => {
						throw new Error("observer failure");
					},
				},
			),
		).resolves.toBeUndefined();
		expect(output.stopReason).toBe("stop");
		expect(output.usage).toMatchObject({ input: 7, cacheRead: 5, output: 8, totalTokens: 20 });
	});

	it("passes observations through OpenAI, Azure, and Codex adapters", async () => {
		const rawUsage = {
			input_tokens: 14,
			input_tokens_details: { cached_tokens: 3 },
			output_tokens: 10,
			output_tokens_details: { reasoning_tokens: 6 },
			total_tokens: 24,
		};
		vi.spyOn(globalThis, "fetch").mockImplementation(async () => sse(rawUsage));
		const codexToken = `test.${Buffer.from(
			JSON.stringify({ "https://api.openai.com/auth": { chatgpt_account_id: "test-account" } }),
		).toString("base64url")}.signature`;
		const cases = [
			{
				model: getModel("openai", "gpt-5.4"),
				run: (observe: (usage: ProviderUsageObservation) => void) =>
					streamOpenAIResponses(getModel("openai", "gpt-5.4"), context, {
						apiKey: "fake",
						onUsageObservation: observe,
					}).result(),
			},
			{
				model: getModel("azure-openai-responses", "gpt-4o-mini"),
				run: (observe: (usage: ProviderUsageObservation) => void) =>
					streamAzureOpenAIResponses(getModel("azure-openai-responses", "gpt-4o-mini"), context, {
						apiKey: "fake",
						azureBaseUrl: "https://fake-resource.openai.azure.com/openai/v1",
						onUsageObservation: observe,
					}).result(),
			},
			{
				model: getModel("openai-codex", "gpt-6-astra"),
				run: (observe: (usage: ProviderUsageObservation) => void) =>
					streamOpenAICodexResponses(getModel("openai-codex", "gpt-6-astra"), context, {
						apiKey: codexToken,
						onUsageObservation: observe,
					}).result(),
			},
		];
		for (const entry of cases) {
			const observed = vi.fn<(usage: ProviderUsageObservation) => void>();
			const result = await entry.run(observed);
			expect(result.stopReason).toBe("stop");
			expect(observed).toHaveBeenCalledWith(
				expect.objectContaining({
					inputTokens: 14,
					cachedInputTokens: 3,
					outputTokens: 10,
					reasoningTokens: 6,
					cachedInputIncludedInInput: true,
					reasoningIncludedInOutput: true,
				}),
				entry.model,
			);
		}
	});

	it("does not serialize the observation callback into an OpenAI request", async () => {
		const bodies: string[] = [];
		vi.spyOn(globalThis, "fetch").mockImplementation(async (_input, init) => {
			bodies.push(String(init?.body));
			return sse({ input_tokens: 1, output_tokens: 1, total_tokens: 2 });
		});
		const model = getModel("openai", "gpt-5.4");
		await streamOpenAIResponses(model, context, { apiKey: "fake" }).result();
		await streamOpenAIResponses(model, context, {
			apiKey: "fake",
			onUsageObservation() {},
		}).result();
		expect(bodies).toHaveLength(2);
		expect(JSON.parse(bodies[0])).toEqual(JSON.parse(bodies[1]));
		expect(bodies[1]).not.toContain("onUsageObservation");
	});
});
