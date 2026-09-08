import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
	closeOpenAICodexWebSocketSessions,
	getOpenAICodexWebSocketDebugStats,
	resetOpenAICodexWebSocketDebugStats,
	streamOpenAICodexResponses,
} from "../src/providers/openai-codex-responses.js";
import { requestOpenAICompaction, supportsOpenAICompaction } from "../src/providers/openai-compaction.js";
import type { Context, Model } from "../src/types.js";

type Listener = (event: unknown) => void;
type SocketPlan = (socket: FakeSocket) => void;

class FakeSocket {
	static instances: FakeSocket[] = [];
	static plans: SocketPlan[] = [];
	readyState = 0;
	readonly payloads: Record<string, unknown>[] = [];
	private readonly listeners = new Map<string, Set<Listener>>();

	constructor() {
		FakeSocket.instances.push(this);
		queueMicrotask(() => {
			this.readyState = 1;
			this.emit("open", {});
		});
	}

	addEventListener(type: string, listener: Listener) {
		const listeners = this.listeners.get(type) ?? new Set<Listener>();
		listeners.add(listener);
		this.listeners.set(type, listeners);
	}

	removeEventListener(type: string, listener: Listener) {
		this.listeners.get(type)?.delete(listener);
	}

	emit(type: string, event: unknown) {
		for (const listener of this.listeners.get(type) ?? []) listener(event);
	}

	message(event: Record<string, unknown>) {
		this.emit("message", { data: JSON.stringify(event) });
	}

	send(data: string) {
		this.payloads.push(JSON.parse(data) as Record<string, unknown>);
		const plan = FakeSocket.plans.shift();
		if (!plan) throw new Error("Unexpected synthetic WebSocket request");
		queueMicrotask(() => plan(this));
	}

	close(code = 1000) {
		this.readyState = 3;
		this.emit("close", { code, wasClean: code === 1000 });
	}
}

const model: Model<"openai-codex-responses"> = {
	id: "gpt-6-astra",
	name: "Astra synthetic",
	provider: "openai-codex",
	api: "openai-codex-responses",
	baseUrl: "https://unused.invalid",
	reasoning: true,
	input: ["text"],
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	contextWindow: 1_050_000,
	maxTokens: 128_000,
};
const context: Context = { messages: [{ role: "user", content: "Synthetic prompt", timestamp: 1 }] };
const token = `test.${Buffer.from(JSON.stringify({ "https://api.openai.com/auth": { chatgpt_account_id: "synthetic" } })).toString("base64url")}.signature`;
const item = {
	type: "message",
	id: "msg_synthetic",
	role: "assistant",
	status: "completed",
	content: [{ type: "output_text", text: "Synthetic result", annotations: [] }],
};
const events: Record<string, unknown>[] = [
	{ type: "response.created", response: { id: "resp_synthetic" } },
	{ type: "response.output_item.added", item: { ...item, content: [] } },
	{ type: "response.content_part.added", part: { type: "output_text", text: "" } },
	{ type: "response.output_text.delta", delta: "Synthetic result" },
	{ type: "response.output_item.done", item },
	{ type: "response.completed", response: { id: "resp_synthetic", status: "completed" } },
];
const complete: SocketPlan = (socket) => {
	for (const event of events) socket.message(event);
};
const fail: SocketPlan = (socket) => socket.close(1006);

beforeEach(() => {
	FakeSocket.instances = [];
	FakeSocket.plans = [];
	vi.stubGlobal("WebSocket", FakeSocket);
	vi.stubGlobal(
		"fetch",
		vi.fn(() => Promise.reject(new Error("Unexpected synthetic fetch"))),
	);
});

afterEach(() => {
	closeOpenAICodexWebSocketSessions();
	resetOpenAICodexWebSocketDebugStats();
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
});

describe("Codex reconnect boundary", () => {
	it("reconnects exactly once before any response event, without falling back", async () => {
		FakeSocket.plans = [fail, complete];
		const callback = vi.fn();
		const result = await streamOpenAICodexResponses(model, context, {
			apiKey: token,
			sessionId: "reconnect",
			transport: "auto",
			onOutputItemDone: callback,
		}).result();
		expect(result.stopReason).toBe("stop");
		expect(FakeSocket.instances).toHaveLength(2);
		expect(fetch).not.toHaveBeenCalled();
		expect(callback).toHaveBeenCalledExactlyOnceWith(item);
		expect(getOpenAICodexWebSocketDebugStats("reconnect")?.sseFallbacks).toBe(0);
	});

	it("uses one SSE fallback after two pre-event failures and keeps the session on SSE", async () => {
		FakeSocket.plans = [fail, fail];
		const payload = events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("");
		vi.stubGlobal(
			"fetch",
			vi.fn(async () => new Response(payload)),
		);
		const options = { apiKey: token, sessionId: "fallback", transport: "auto" as const };
		expect((await streamOpenAICodexResponses(model, context, options).result()).stopReason).toBe("stop");
		expect(FakeSocket.instances).toHaveLength(2);
		expect(fetch).toHaveBeenCalledTimes(1);
		expect((await streamOpenAICodexResponses(model, context, options).result()).stopReason).toBe("stop");
		expect(FakeSocket.instances).toHaveLength(2);
		expect(fetch).toHaveBeenCalledTimes(2);
		expect(getOpenAICodexWebSocketDebugStats("fallback")?.websocketFallbackActive).toBe(true);
	});

	it.each([1, 4])("does not reconnect or replay after %s provider events", async (count) => {
		FakeSocket.plans = [
			(socket) => {
				for (const event of events.slice(0, count)) socket.message(event);
				setTimeout(() => socket.close(1006), 0);
			},
		];
		const result = await streamOpenAICodexResponses(model, context, {
			apiKey: token,
			sessionId: `partial-${count}`,
		}).result();
		expect(result.stopReason).toBe("error");
		expect(FakeSocket.instances).toHaveLength(1);
		expect(fetch).not.toHaveBeenCalled();
		expect(result.diagnostics?.some((entry) => entry.details?.eventsEmitted === true)).toBe(true);
	});

	it("does not reconnect after provider rejection", async () => {
		FakeSocket.plans = [(socket) => socket.message({ type: "error", code: "invalid_request", message: "Rejected" })];
		const result = await streamOpenAICodexResponses(model, context, { apiKey: token }).result();
		expect(result.stopReason).toBe("error");
		expect(FakeSocket.instances).toHaveLength(1);
		expect(fetch).not.toHaveBeenCalled();
	});

	it("retains cached incremental requests and sends full context after a failed cached socket", async () => {
		FakeSocket.plans = [complete, fail, complete];
		const options = { apiKey: token, sessionId: "cached", transport: "websocket-cached" as const };
		const first = await streamOpenAICodexResponses(model, context, options).result();
		const followup: Context = {
			messages: [...context.messages, first, { role: "user", content: "Follow-up", timestamp: 2 }],
		};
		const second = await streamOpenAICodexResponses(model, followup, options).result();
		expect(second.stopReason).toBe("stop");
		expect(FakeSocket.instances).toHaveLength(2);
		expect(FakeSocket.instances[0].payloads[1].previous_response_id).toBe("resp_synthetic");
		expect(FakeSocket.instances[0].payloads[1].input).toHaveLength(1);
		expect(FakeSocket.instances[1].payloads[0].previous_response_id).toBeUndefined();
		expect(FakeSocket.instances[1].payloads[0].input).toHaveLength(3);
		expect(fetch).not.toHaveBeenCalled();
	});

	it("never converts cancellation into a reconnect", async () => {
		const controller = new AbortController();
		FakeSocket.plans = [() => controller.abort()];
		const result = await streamOpenAICodexResponses(model, context, {
			apiKey: token,
			signal: controller.signal,
		}).result();
		expect(result.stopReason).toBe("aborted");
		expect(FakeSocket.instances).toHaveLength(1);
		expect(fetch).not.toHaveBeenCalled();
	});

	it("forwards completed output-item hooks on SSE", async () => {
		vi.stubGlobal(
			"fetch",
			vi.fn(async () => new Response(events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join(""))),
		);
		const callback = vi.fn();
		const result = await streamOpenAICodexResponses(model, context, {
			apiKey: token,
			transport: "sse",
			onOutputItemDone: callback,
		}).result();
		expect(result.stopReason).toBe("stop");
		expect(callback).toHaveBeenCalledExactlyOnceWith(item);
		expect(FakeSocket.instances).toHaveLength(0);
	});
});

describe("native compaction allowance", () => {
	it("keeps all Codex models eligible without inventing routes for other providers", () => {
		expect(supportsOpenAICompaction({ ...model, id: "gpt-5.6-sol" })).toBe(true);
		expect(supportsOpenAICompaction({ ...model, provider: "github-copilot" })).toBe(false);
		expect(
			supportsOpenAICompaction({ ...model, provider: "azure-openai-responses", api: "azure-openai-responses" }),
		).toBe(false);
		expect(supportsOpenAICompaction({ ...model, provider: "ollama" })).toBe(false);
		expect(supportsOpenAICompaction({ ...model, provider: "openai", api: "openai-responses" })).toBe(true);
		expect(
			supportsOpenAICompaction({ ...model, id: "gpt-5.6-sol", provider: "openai", api: "openai-responses" }),
		).toBe(false);
	});

	it.each([undefined, 1234])("uses the twenty-minute default unless explicitly overridden: %s", async (timeoutMs) => {
		const timeout = vi.spyOn(AbortSignal, "timeout").mockImplementation(() => new AbortController().signal);
		vi.stubGlobal(
			"fetch",
			vi.fn(async () => Response.json({ output: [{ type: "compaction", encrypted_content: "opaque" }] })),
		);
		const result = await requestOpenAICompaction(
			model,
			"https://unused.invalid/compact",
			new Headers(),
			{},
			{ timeoutMs },
		);
		expect(timeout).toHaveBeenCalledExactlyOnceWith(timeoutMs ?? 1_200_000);
		expect(result?.checkpoint.items).toHaveLength(1);
	});
});
