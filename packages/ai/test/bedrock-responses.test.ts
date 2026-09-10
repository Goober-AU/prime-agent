import { Type } from "typebox";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { getModel, getModelInputLimit, getSupportedThinkingLevels, supportsFastMode } from "../src/models.js";
import { streamBedrockResponses, streamSimpleBedrockResponses } from "../src/providers/amazon-bedrock-responses.js";
import { compactSimple, streamSimple, supportsCompaction } from "../src/stream.js";
import type { AssistantMessage, Context } from "../src/types.js";

const routes = [
	{
		id: "openai.gpt-6-astra",
		region: "us-west-2",
		service: "bedrock-mantle",
		host: "bedrock-mantle.us-west-2.api.aws",
	},
	{
		id: "global.openai.gpt-6-astra",
		region: "ap-southeast-2",
		service: "bedrock",
		host: "bedrock-runtime.ap-southeast-2.amazonaws.com",
	},
] as const;
const context: Context = {
	systemPrompt: "Use tools precisely.",
	messages: [{ role: "user", content: "Hello", timestamp: 1 }],
	tools: [{ name: "lookup", description: "Look up a value", parameters: Type.Object({ key: Type.String() }) }],
};
const requests: { url: string; headers: Headers; body: Record<string, unknown>; redirect: Request["redirect"] }[] = [];
const reasoningItem = { type: "reasoning", id: "rs_test", summary: [], encrypted_content: "opaque-reasoning" };
const toolItem = {
	type: "function_call",
	id: "fc_test",
	call_id: "call_test",
	name: "lookup",
	arguments: '{"key":"value"}',
};
const textItem = {
	type: "message",
	id: "msg_test",
	role: "assistant",
	status: "completed",
	content: [{ type: "output_text", text: "Hello", annotations: [] }],
};

function sse(items: object[] = [textItem], inputTokens = 100) {
	const events = [
		{ type: "response.created", response: { id: "resp_test" } },
		...items.flatMap((item) => [
			{ type: "response.output_item.added", item },
			{ type: "response.output_item.done", item },
		]),
		{
			type: "response.completed",
			response: {
				id: "resp_test",
				status: "completed",
				usage: {
					input_tokens: inputTokens,
					input_tokens_details: { cached_tokens: 20 },
					output_tokens: 10,
					total_tokens: inputTokens + 10,
				},
			},
		},
	];
	return new Response(events.map((event) => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join(""), {
		headers: { "content-type": "text/event-stream", "x-request-id": "aws-test" },
	});
}

beforeEach(() => {
	requests.length = 0;
	for (const name of [
		"AWS_BEARER_TOKEN_BEDROCK",
		"AWS_BEDROCK_BASE_URL",
		"AWS_PROFILE",
		"AWS_ACCESS_KEY_ID",
		"AWS_SECRET_ACCESS_KEY",
		"AWS_SESSION_TOKEN",
		"AWS_REGION",
		"AWS_DEFAULT_REGION",
	])
		vi.stubEnv(name, "");
	vi.stubGlobal(
		"fetch",
		vi.fn(async (input: Parameters<typeof fetch>[0], init?: RequestInit) => {
			const request = new Request(input, init);
			requests.push({
				url: request.url,
				headers: request.headers,
				body: JSON.parse(await request.text()),
				redirect: request.redirect,
			});
			return sse();
		}),
	);
});
afterEach(() => {
	vi.unstubAllEnvs();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe.each(routes)("Bedrock Astra $id", (route) => {
	const model = getModel("amazon-bedrock", route.id);
	it("registers the correct route, effort levels, and conservative input budget", async () => {
		expect(model.api).toBe("bedrock-responses");
		expect(new URL(model.baseUrl).hostname).toBe(route.host);
		expect(model.input).toEqual(["text", "image"]);
		expect(model.contextWindow).toBe(1_050_000);
		expect(model.maxTokens).toBe(128_000);
		expect(getModelInputLimit(model)).toBe(922_000);
		expect(getSupportedThinkingLevels(model)).toEqual(["low", "medium", "high", "xhigh", "max"]);
		expect(supportsFastMode(model)).toBe(false);
		expect(supportsCompaction(model)).toBe(false);
		expect(await compactSimple(model, context)).toBeUndefined();
		expect(requests).toHaveLength(0);
	});
	it.each(["low", "medium", "high", "xhigh", "max"] as const)(
		"sends %s reasoning over authenticated Responses",
		async (effort) => {
			vi.stubEnv("OPENAI_API_KEY", "must-not-use-openai-key");
			vi.stubEnv("AWS_REGION", "eu-west-1"); // The named catalog route owns the endpoint region.
			vi.stubEnv("AWS_BEARER_TOKEN_BEDROCK", "aws-token");
			const observer = vi.fn();
			const onResponse = vi.fn();
			const result = await streamSimpleBedrockResponses(model, context, {
				reasoning: effort,
				onUsageObservation: observer,
				onResponse,
				maxTokens: 1000,
			}).result();
			expect(result.stopReason, result.errorMessage).toBe("stop");
			expect(result.content[0]).toMatchObject({ type: "text", text: "Hello" });
			expect(result.provider).toBe("amazon-bedrock");
			expect(result.api).toBe("bedrock-responses");
			expect(result.model).toBe(route.id);
			expect(requests[0].url).toBe(`https://${route.host}/openai/v1/responses`);
			expect(requests[0].headers.get("authorization")).toBe("Bearer aws-token");
			expect(requests[0].redirect).toBe("manual");
			expect(requests[0].body).toMatchObject({
				model: route.id,
				stream: true,
				store: false,
				service_tier: "default",
				max_output_tokens: 1000,
				reasoning: { effort },
				include: ["reasoning.encrypted_content"],
				tools: [{ type: "function", name: "lookup" }],
			});
			expect(requests[0].body).not.toHaveProperty("prompt_cache_retention");
			expect(requests[0].body).not.toHaveProperty("temperature");
			expect(result.usage).toMatchObject({ input: 80, output: 10, cacheRead: 20, totalTokens: 110 });
			expect(observer).toHaveBeenCalledOnce();
			expect(onResponse).toHaveBeenCalledWith(expect.objectContaining({ status: 200 }), model);
		},
	);
	it("signs with the endpoint's AWS service and region, including temporary credentials", async () => {
		const credentialProvider = vi.fn(async () => ({
			accessKeyId: "AKIATEST",
			secretAccessKey: "test-secret",
			sessionToken: "test-session",
		}));
		vi.stubEnv("AWS_BEARER_TOKEN_BEDROCK", "ignored-for-explicit-iam");
		const result = await streamBedrockResponses(model, context, {
			apiKey: "<authenticated>",
			credentialProvider,
		}).result();
		expect(result.stopReason, result.errorMessage).toBe("stop");
		expect(credentialProvider).toHaveBeenCalled();
		expect(requests[0].headers.get("authorization")).toMatch(
			new RegExp(`Credential=AKIATEST/\\d{8}/${route.region}/${route.service}/aws4_request`),
		);
		expect(requests[0].headers.get("x-amz-security-token")).toBe("test-session");
		expect(requests[0].headers.get("x-amz-content-sha256")).toMatch(/^[a-f0-9]{64}$/);
	});
	it("uses ambient IAM credentials for the app's authentication sentinel", async () => {
		vi.stubEnv("AWS_ACCESS_KEY_ID", "AKIAAMBIENT");
		vi.stubEnv("AWS_SECRET_ACCESS_KEY", "ambient-secret");
		const result = await streamSimpleBedrockResponses(model, context, { apiKey: "<authenticated>" }).result();
		expect(result.stopReason, result.errorMessage).toBe("stop");
		expect(requests[0].headers.get("authorization")).toContain(`/${route.region}/${route.service}/aws4_request`);
		expect(requests[0].headers.get("authorization")).not.toContain("<authenticated>");
	});
	it("replays encrypted reasoning and paired tool IDs after JSON session persistence", async () => {
		vi.mocked(fetch).mockResolvedValueOnce(sse([reasoningItem, toolItem]));
		const first = await streamBedrockResponses(model, context, { apiKey: "aws-test" }).result();
		expect(first.stopReason, first.errorMessage).toBe("toolUse");
		const saved = JSON.parse(JSON.stringify(first)) as AssistantMessage;
		const followup: Context = {
			...context,
			messages: [
				...context.messages,
				saved,
				{
					role: "toolResult",
					toolCallId: "call_test|fc_test",
					toolName: "lookup",
					content: [{ type: "text", text: "found" }],
					isError: false,
					timestamp: 2,
				},
			],
		};
		const result = await streamBedrockResponses(model, followup, {
			apiKey: "aws-test",
			reasoningEffort: "max",
		}).result();
		expect(result.stopReason, result.errorMessage).toBe("stop");
		expect(requests[0].body.input).toEqual(
			expect.arrayContaining([
				reasoningItem,
				expect.objectContaining({ type: "function_call", call_id: "call_test", id: "fc_test" }),
				{ type: "function_call_output", call_id: "call_test", output: "found" },
			]),
		);
	});
	it("includes user images", async () => {
		const image = { type: "image" as const, data: "aW1hZ2U=", mimeType: "image/png" };
		const input: Context = {
			messages: [{ role: "user", content: [image, { type: "text", text: "describe" }], timestamp: 1 }],
		};
		await streamBedrockResponses(model, input, { apiKey: "aws-test" }).result();
		expect(requests[0].body.input).toEqual([
			{
				role: "user",
				content: [
					{ type: "input_image", detail: "auto", image_url: "data:image/png;base64,aW1hZ2U=" },
					{ type: "input_text", text: "describe" },
				],
			},
		]);
	});
	it("dispatches through the registered lazy provider", async () => {
		const result = await streamSimple(model, context, { apiKey: "aws-test", reasoning: "max" }).result();
		expect(result.stopReason, result.errorMessage).toBe("stop");
		expect(requests[0].body.reasoning).toMatchObject({ effort: "max" });
	});
	it("rejects unsupported tiers before sending a request", async () => {
		const result = await streamBedrockResponses(model, context, {
			apiKey: "aws-test",
			serviceTier: "priority",
		}).result();
		expect(result.errorMessage).toContain("Standard");
		expect(requests).toHaveLength(0);
	});
	it("reports provider errors without transport retries", async () => {
		vi.mocked(fetch).mockResolvedValue(
			new Response(JSON.stringify({ error: { message: "Access denied", type: "permission_error" } }), {
				status: 403,
				headers: { "content-type": "application/json" },
			}),
		);
		const result = await streamBedrockResponses(model, context, { apiKey: "aws-test" }).result();
		expect(result.stopReason).toBe("error");
		expect(result.errorMessage).toContain("Access denied");
		expect(fetch).toHaveBeenCalledTimes(1);
	});
	it("honors cancellation before sending", async () => {
		const controller = new AbortController();
		controller.abort();
		const result = await streamBedrockResponses(model, context, {
			apiKey: "aws-test",
			signal: controller.signal,
		}).result();
		expect(result.stopReason).toBe("aborted");
		expect(fetch).not.toHaveBeenCalled();
	});
	it("charges the documented long-context rates", async () => {
		vi.mocked(fetch).mockResolvedValue(sse([textItem], 300_000));
		const result = await streamBedrockResponses(model, context, { apiKey: "aws-test" }).result();
		expect(result.usage.cost.input).toBeCloseTo((299_980 * model.cost.input * 2) / 1e6);
		expect(result.usage.cost.output).toBeCloseTo((10 * model.cost.output * 1.5) / 1e6);
		expect(result.usage.cost.cacheRead).toBeCloseTo((20 * model.cost.cacheRead * 2) / 1e6);
	});
});

it("rejects a Sydney Mantle URL instead of silently changing the selected route", async () => {
	const model = getModel("amazon-bedrock", "openai.gpt-6-astra");
	const result = await streamBedrockResponses(
		{ ...model, baseUrl: "https://bedrock-mantle.ap-southeast-2.api.aws/openai/v1" },
		context,
		{ apiKey: "aws-test" },
	).result();
	expect(result.errorMessage).toContain("Oregon");
	expect(fetch).not.toHaveBeenCalled();
});

it("drops foreign encrypted reasoning while keeping tool results when switching Oregon to Sydney", async () => {
	const oregon = getModel("amazon-bedrock", "openai.gpt-6-astra");
	const sydney = getModel("amazon-bedrock", "global.openai.gpt-6-astra");
	vi.mocked(fetch).mockResolvedValueOnce(sse([reasoningItem, toolItem]));
	const first = await streamBedrockResponses(oregon, context, { apiKey: "test" }).result();
	const next: Context = {
		messages: [
			...context.messages,
			first,
			{
				role: "toolResult",
				toolCallId: "call_test|fc_test",
				toolName: "lookup",
				content: [{ type: "image", data: "aW1hZ2U=", mimeType: "image/png" }],
				isError: false,
				timestamp: 2,
			},
		],
	};
	const result = await streamBedrockResponses(sydney, next, { apiKey: "test" }).result();
	expect(result.stopReason, result.errorMessage).toBe("stop");
	expect(requests[0].body.input).not.toEqual(expect.arrayContaining([expect.objectContaining({ type: "reasoning" })]));
	expect(requests[0].body.input).toEqual(
		expect.arrayContaining([
			expect.objectContaining({ type: "function_call", call_id: "call_test" }),
			{
				type: "function_call_output",
				call_id: "call_test",
				output: [{ type: "input_image", detail: "auto", image_url: "data:image/png;base64,aW1hZ2U=" }],
			},
		]),
	);
});

it("rejects a mismatched explicit signing region", async () => {
	const result = await streamBedrockResponses(getModel("amazon-bedrock", "global.openai.gpt-6-astra"), context, {
		apiKey: "test",
		region: "us-west-2",
	}).result();
	expect(result.errorMessage).toContain("does not match signing region");
	expect(fetch).not.toHaveBeenCalled();
});

it("does not forward authorization across redirects", async () => {
	vi.mocked(fetch).mockResolvedValue(
		new Response(null, { status: 307, headers: { location: "https://outside.example/responses" } }),
	);
	const result = await streamBedrockResponses(getModel("amazon-bedrock", "openai.gpt-6-astra"), context, {
		apiKey: "test",
	}).result();
	expect(result.stopReason).toBe("error");
	expect(fetch).toHaveBeenCalledTimes(1);
	expect(vi.mocked(fetch).mock.calls[0][0]).toMatchObject({ redirect: "manual" });
});
