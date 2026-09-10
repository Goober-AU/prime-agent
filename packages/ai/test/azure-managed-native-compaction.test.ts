import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { afterEach, describe, expect, it, vi } from "vitest";
import { compactionMatchesModel, isCompactionCheckpoint } from "../src/compaction.js";
import { getModel } from "../src/models.js";
import { streamOpenAIResponses } from "../src/providers/openai-responses.js";
import { convertResponsesMessages } from "../src/providers/openai-responses-shared.js";
import { compactSimple, supportsCompaction } from "../src/stream.js";
import type { AssistantMessage, Context, Model } from "../src/types.js";

const context: Context = {
	systemPrompt: "Preserve every constraint.",
	messages: [{ role: "user", content: "Remember artifact C:/work/evidence.json", timestamp: 1 }],
};
const opaqueOutput = [{ type: "compaction", id: "cmp_azure", encrypted_content: "opaque-azure-checkpoint" }];
const servers = new Set<ReturnType<typeof createServer>>();

function azureModel(baseUrl: string, overrides: Partial<Model<"openai-responses">> = {}): Model<"openai-responses"> {
	return {
		id: "gpt-6-astra",
		name: "Azure managed Astra",
		api: "openai-responses",
		provider: "azure-openai-managed",
		baseUrl: `${baseUrl}/azure-openai/v1`,
		reasoning: true,
		input: ["text", "image"],
		cost: { input: 1, output: 2, cacheRead: 0.5, cacheWrite: 0 },
		contextWindow: 1_050_000,
		maxTokens: 128_000,
		nativeCompaction: {
			protocol: "openai-responses-compact-v1",
			provider: "azure-openai-managed",
			model: "gpt-6-astra",
			endpoint: `${baseUrl}/azure-openai/v1/responses/compact`,
			apiVersion: "v1",
			enabled: true,
			validation: "live-verified",
		},
		...overrides,
	};
}

function toolAssistant(provider: string, api: AssistantMessage["api"], model: string, id: string): AssistantMessage {
	return {
		role: "assistant",
		content: [{ type: "toolCall", id, name: "lookup", arguments: { exact: true } }],
		api,
		provider,
		model,
		usage: {
			input: 1,
			output: 1,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 2,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "toolUse",
		timestamp: 2,
	};
}

function compositeToolContext(model: Model<"openai-responses">): Context {
	return {
		systemPrompt: "Preserve tool pairing.",
		messages: [
			{ role: "user", content: "Run lookups", timestamp: 1 },
			toolAssistant(model.provider, model.api, model.id, "call_current|fc_current"),
			{
				role: "toolResult",
				toolCallId: "call_current|fc_current",
				toolName: "lookup",
				content: [{ type: "text", text: "current" }],
				isError: false,
				timestamp: 3,
			},
			toolAssistant(model.provider, model.api, "gpt-previous", "call_changed|fc_changed"),
			{
				role: "toolResult",
				toolCallId: "call_changed|fc_changed",
				toolName: "lookup",
				content: [{ type: "text", text: "changed" }],
				isError: false,
				timestamp: 4,
			},
			toolAssistant("foreign-provider", model.api, "foreign-model", "call_foreign|foreign_item"),
			{
				role: "toolResult",
				toolCallId: "call_foreign|foreign_item",
				toolName: "lookup",
				content: [{ type: "text", text: "foreign" }],
				isError: false,
				timestamp: 5,
			},
		],
	};
}

async function readRequest(request: IncomingMessage): Promise<string> {
	const chunks: Buffer[] = [];
	for await (const chunk of request) chunks.push(Buffer.from(chunk));
	return Buffer.concat(chunks).toString("utf8");
}

async function startServer(
	handler: (request: IncomingMessage, response: ServerResponse) => void | Promise<void>,
): Promise<{ baseUrl: string; server: ReturnType<typeof createServer> }> {
	const server = createServer((request, response) => {
		void Promise.resolve(handler(request, response)).catch((error) => response.destroy(error as Error));
	});
	servers.add(server);
	await new Promise<void>((resolve, reject) => {
		server.once("error", reject);
		server.listen(0, "127.0.0.1", resolve);
	});
	const { port } = server.address() as AddressInfo;
	return { baseUrl: `http://127.0.0.1:${port}`, server };
}

function sendJson(
	response: ServerResponse,
	status: number,
	payload: unknown,
	headers: Record<string, string> = {},
): void {
	response.writeHead(status, { "content-type": "application/json", ...headers });
	response.end(JSON.stringify(payload));
}

afterEach(async () => {
	vi.restoreAllMocks();
	await Promise.all(
		[...servers].map(
			(server) =>
				new Promise<void>((resolve) => {
					server.closeAllConnections?.();
					server.close(() => resolve());
				}),
		),
	);
	servers.clear();
});

describe("explicit Azure managed Astra native compaction", () => {
	it("stays off unless every provider/model/endpoint/validation field matches", () => {
		const base = "http://127.0.0.1:43119";
		const valid = azureModel(base);
		expect(supportsCompaction(valid)).toBe(true);
		for (const model of [
			{ ...valid, nativeCompaction: undefined },
			{ ...valid, nativeCompaction: { ...valid.nativeCompaction!, enabled: false } },
			{ ...valid, nativeCompaction: { ...valid.nativeCompaction!, validation: "unverified" as const } },
			{ ...valid, nativeCompaction: { ...valid.nativeCompaction!, validation: "documentation-verified" as const } },
			{ ...valid, nativeCompaction: { ...valid.nativeCompaction!, provider: "openai" } },
			{ ...valid, nativeCompaction: { ...valid.nativeCompaction!, model: "gpt-5.6-sol" } },
			{ ...valid, nativeCompaction: { ...valid.nativeCompaction!, endpoint: `${base}/other/responses/compact` } },
			{ ...valid, baseUrl: `${base}/azure-openai/preview` },
			{ ...valid, provider: "github-copilot" },
			{ ...valid, provider: "azure-foundry" },
			{ ...valid, provider: "ollama" },
		]) {
			expect(supportsCompaction(model as Model<"openai-responses">)).toBe(false);
		}
	});

	it("uses the real Responses serializer over HTTP and persists an endpoint-owned opaque checkpoint", async () => {
		let received: { url: string; authorization: string | undefined; body: Record<string, unknown> } | undefined;
		const { baseUrl } = await startServer(async (request, response) => {
			received = {
				url: request.url ?? "",
				authorization: request.headers.authorization,
				body: JSON.parse(await readRequest(request)) as Record<string, unknown>,
			};
			sendJson(response, 200, {
				output: opaqueOutput,
				usage: {
					input_tokens: 100,
					input_tokens_details: { cached_tokens: 25 },
					output_tokens: 12,
					total_tokens: 112,
				},
			});
		});
		const model = azureModel(baseUrl);
		const result = await compactSimple(model, context, {
			apiKey: "fake-local-token",
			sessionId: "session-azure",
			customInstructions: "Keep exact errors",
			reasoning: "max",
		});
		expect(received).toMatchObject({
			url: "/azure-openai/v1/responses/compact",
			authorization: "Bearer fake-local-token",
			body: {
				model: "gpt-6-astra",
				instructions: "Preserve every constraint.\n\nKeep exact errors",
				prompt_cache_key: "session-azure",
			},
		});
		expect(received?.body.input).toEqual([
			{ role: "user", content: [{ type: "input_text", text: "Remember artifact C:/work/evidence.json" }] },
		]);
		expect(result?.checkpoint).toMatchObject({
			provider: "azure-openai-managed",
			model: "gpt-6-astra",
			baseUrl: `${baseUrl}/azure-openai/v1`,
			endpoint: `${baseUrl}/azure-openai/v1/responses/compact`,
			items: opaqueOutput,
		});
		expect(result?.usage).toMatchObject({ input: 75, cacheRead: 25, output: 12, totalTokens: 112 });
		const reloaded = JSON.parse(JSON.stringify(result?.checkpoint));
		expect(isCompactionCheckpoint(reloaded)).toBe(true);
		expect(compactionMatchesModel(reloaded, model)).toBe(true);
		expect(
			compactionMatchesModel(reloaded, {
				...model,
				nativeCompaction: { ...model.nativeCompaction!, endpoint: `${baseUrl}/different/responses/compact` },
			}),
		).toBe(false);

		const replay = convertResponsesMessages(
			model,
			{
				messages: [
					{ role: "user", content: "display-only marker", providerContext: reloaded, timestamp: 2 },
					{ role: "user", content: "continue", timestamp: 3 },
				],
			},
			new Set(),
		);
		expect(replay[0]).toEqual(opaqueOutput[0]);
		expect(JSON.stringify(replay)).not.toContain("display-only marker");
	});

	it("keeps Responses composite tool IDs paired in the validated compact serializer", async () => {
		let body: Record<string, unknown> | undefined;
		const { baseUrl } = await startServer(async (request, response) => {
			body = JSON.parse(await readRequest(request)) as Record<string, unknown>;
			sendJson(response, 200, { output: opaqueOutput });
		});
		const model = azureModel(baseUrl);
		await compactSimple(model, compositeToolContext(model), { apiKey: "fake" });

		const input = body?.input as Array<Record<string, unknown>>;
		const calls = input.filter((item) => item.type === "function_call");
		const outputs = input.filter((item) => item.type === "function_call_output");
		expect(calls).toHaveLength(3);
		expect(outputs).toHaveLength(3);
		expect(calls[0]).toMatchObject({ id: "fc_current", call_id: "call_current", name: "lookup" });
		expect(outputs[0]).toMatchObject({ call_id: "call_current", output: "current" });
		expect(calls[1]).toMatchObject({ call_id: "call_changed", name: "lookup" });
		expect(calls[1]).not.toHaveProperty("id");
		expect(outputs[1]).toMatchObject({ call_id: "call_changed", output: "changed" });
		expect(calls[2]?.call_id).toBe("call_foreign");
		expect(calls[2]?.id).toMatch(/^fc_[a-z0-9]+$/);
		expect(calls[2]?.id).not.toBe("foreign_item");
		expect(outputs[2]).toMatchObject({ call_id: "call_foreign", output: "foreign" });
	});

	it.each([404, 405, 501])("falls back only for explicit unsupported HTTP %s", async (status) => {
		const { baseUrl } = await startServer((_request, response) =>
			sendJson(response, status, { error: "unsupported" }),
		);
		expect(await compactSimple(azureModel(baseUrl), context, { apiKey: "fake" })).toBeUndefined();
	});

	it.each([401, 403, 429, 500])("keeps HTTP %s as a failure", async (status) => {
		const { baseUrl } = await startServer((_request, response) =>
			sendJson(response, status, { error: { message: "sensitive echoed body" } }, { "retry-after": "2" }),
		);
		await expect(compactSimple(azureModel(baseUrl), context, { apiKey: "fake" })).rejects.toMatchObject({
			status,
			retryAfterMs: 2000,
			message: `Server compaction failed (HTTP ${status})`,
		});
	});

	it.each([
		{ status: 400, payload: { error: { code: "context_length_exceeded" } } },
		{ status: 413, payload: { error: { code: "request_too_large" } } },
	])("does not disguise Azure request validation as unsupported ($status)", async ({ status, payload }) => {
		const { baseUrl } = await startServer((_request, response) => sendJson(response, status, payload));
		await expect(compactSimple(azureModel(baseUrl), context, { apiKey: "fake" })).rejects.toMatchObject({ status });
	});

	it.each([
		{},
		{ output: [] },
		{ output: [{ type: "compaction", encrypted_content: "" }] },
		{ output: [{ type: "message", content: [] }] },
	])("rejects malformed checkpoint output %#", async (payload) => {
		const { baseUrl } = await startServer((_request, response) => sendJson(response, 200, payload));
		await expect(compactSimple(azureModel(baseUrl), context, { apiKey: "fake" })).rejects.toThrow(
			/valid encrypted checkpoint/,
		);
	});

	it("rejects malformed and partial JSON instead of replaying or falling back", async () => {
		const malformed = await startServer((_request, response) => {
			response.writeHead(200, { "content-type": "application/json" });
			response.end('{"output":');
		});
		await expect(compactSimple(azureModel(malformed.baseUrl), context, { apiKey: "fake" })).rejects.toThrow();

		const partial = await startServer((_request, response) => {
			response.writeHead(200, { "content-type": "application/json" });
			response.write('{"output":[{"type":"compaction"');
			response.destroy(new Error("partial response"));
		});
		await expect(compactSimple(azureModel(partial.baseUrl), context, { apiKey: "fake" })).rejects.toThrow();
	});

	it("preserves cancellation and timeout failures", async () => {
		let requestArrivedResolve!: () => void;
		const requestArrived = new Promise<void>((resolve) => {
			requestArrivedResolve = resolve;
		});
		let cancelledSeenResolve!: () => void;
		const cancelledSeen = new Promise<void>((resolve) => {
			cancelledSeenResolve = resolve;
		});
		const cancellationServer = await startServer((_request, response) => {
			requestArrivedResolve();
			response.once("close", cancelledSeenResolve);
		});
		const abort = new AbortController();
		const pending = compactSimple(azureModel(cancellationServer.baseUrl), context, {
			apiKey: "fake",
			signal: abort.signal,
		});
		await requestArrived;
		abort.abort();
		await expect(pending).rejects.toMatchObject({ name: "AbortError" });
		await cancelledSeen;

		const timeoutServer = await startServer(() => new Promise<void>(() => {}));
		await expect(
			compactSimple(azureModel(timeoutServer.baseUrl), context, { apiKey: "fake", timeoutMs: 10 }),
		).rejects.toThrow();
	});

	it("binds checkpoint ownership to provider, model, base URL, and compact endpoint", () => {
		const baseUrl = "http://127.0.0.1:43119";
		const model = azureModel(baseUrl);
		const checkpoint = {
			version: 1 as const,
			provider: model.provider,
			api: model.api,
			model: model.id,
			baseUrl: model.baseUrl,
			endpoint: model.nativeCompaction!.endpoint,
			items: opaqueOutput,
			estimatedTokens: 10,
		};
		expect(isCompactionCheckpoint(checkpoint)).toBe(true);
		expect(compactionMatchesModel({ ...checkpoint, provider: "openai" }, model)).toBe(false);
		expect(compactionMatchesModel({ ...checkpoint, model: "gpt-5.6-sol" }, model)).toBe(false);
		expect(compactionMatchesModel({ ...checkpoint, baseUrl: `${baseUrl}/other` }, model)).toBe(false);
		expect(compactionMatchesModel({ ...checkpoint, endpoint: `${baseUrl}/other/compact` }, model)).toBe(false);
		expect(compactionMatchesModel(checkpoint, { ...model, nativeCompaction: undefined })).toBe(false);
		expect(isCompactionCheckpoint({ ...checkpoint, endpoint: 42 })).toBe(false);
		const legacyDirect = { ...checkpoint, provider: "openai", baseUrl: "https://api.openai.com/v1" };
		delete (legacyDirect as { endpoint?: string }).endpoint;
		// Direct v1 checkpoints remain readable and keep their endpoint-less schema.
		expect(isCompactionCheckpoint(legacyDirect)).toBe(true);
	});

	it("leaves ordinary Responses route serialization identical with capability absent or present", async () => {
		const bodies: string[] = [];
		vi.spyOn(globalThis, "fetch").mockImplementation(async (_input, init) => {
			bodies.push(String(init?.body));
			return new Response(
				`data: ${JSON.stringify({
					type: "response.completed",
					response: { id: "resp_normal", status: "completed", usage: { input_tokens: 1, output_tokens: 1 } },
				})}\n\ndata: [DONE]\n\n`,
				{ status: 200, headers: { "content-type": "text/event-stream" } },
			);
		});
		const enabled = azureModel("http://127.0.0.1:43119");
		const absent = { ...enabled, nativeCompaction: undefined };
		await streamOpenAIResponses(absent, compositeToolContext(absent), { apiKey: "fake" }).result();
		await streamOpenAIResponses(enabled, compositeToolContext(enabled), { apiKey: "fake" }).result();
		expect(bodies).toHaveLength(2);
		expect(JSON.parse(bodies[0])).toEqual(JSON.parse(bodies[1]));
	});

	it("does not widen direct Codex/OpenAI support or enable other provider contracts", () => {
		expect(supportsCompaction(getModel("openai-codex", "gpt-6-astra"))).toBe(true);
		expect(supportsCompaction(getModel("openai", "gpt-6-astra"))).toBe(true);
		expect(supportsCompaction(getModel("openai", "gpt-5.5"))).toBe(false);
		const exact = azureModel("http://127.0.0.1:43119");
		for (const provider of ["github-copilot", "azure-foundry", "ollama", "openai-compatible"]) {
			expect(supportsCompaction({ ...exact, provider })).toBe(false);
		}
	});
});
