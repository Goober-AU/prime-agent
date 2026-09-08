import type * as ai from "@earendil-works/pi-ai";
import type { Api, AssistantMessage, Model } from "@earendil-works/pi-ai";
import { completeSimple } from "@earendil-works/pi-ai";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
	DEFAULT_COMPACTION_SETTINGS,
	generateSummary,
	shouldCompact,
	shouldCompactForModel,
} from "../src/core/compaction/compaction.js";
import { emptyUsage } from "../src/core/usage.js";

vi.mock("@earendil-works/pi-ai", async (importOriginal) => ({
	...(await importOriginal<typeof ai>()),
	completeSimple: vi.fn(),
}));
const model: Model<Api> = {
	id: "test-large-model",
	name: "Test",
	provider: "test",
	api: "openai-responses",
	baseUrl: "https://example.invalid",
	reasoning: true,
	input: ["text"],
	contextWindow: 1_000_000,
	maxTokens: 128_000,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
};
const reply = (text: string, stopReason: AssistantMessage["stopReason"] = "stop"): AssistantMessage => ({
	role: "assistant",
	content: [{ type: "text", text }],
	api: model.api,
	model: model.id,
	provider: model.provider,
	usage: emptyUsage(),
	timestamp: 0,
	stopReason,
});
const summarize = () =>
	generateSummary(
		[{ role: "user", content: "synthetic history", timestamp: 0 }],
		model,
		16384,
		"test-only",
		undefined,
		undefined,
		undefined,
		undefined,
		"max",
		{ enabled: true, maxRetries: 1, baseDelayMs: 0, maxRetryDelayMs: 1000 },
	);

describe("250K automatic compaction policy", () => {
	it.each([249999, 250000, 250001])("has an inclusive boundary at %d", (tokens) => {
		expect(shouldCompact(tokens, model.contextWindow, DEFAULT_COMPACTION_SETTINGS)).toBe(tokens >= 250000);
	});
	it("keeps smaller-window reserves and the original model window", () => {
		const small = { ...model, contextWindow: 128000 };
		const threshold = small.contextWindow - DEFAULT_COMPACTION_SETTINGS.reserveTokens;
		expect(shouldCompactForModel(threshold - 1, small, DEFAULT_COMPACTION_SETTINGS)).toBe(false);
		expect(shouldCompactForModel(threshold, small, DEFAULT_COMPACTION_SETTINGS)).toBe(true);
		expect(model.contextWindow).toBe(1000000);
		expect(small.contextWindow).toBe(128000);
	});
	it("uses the same policy for Codex Astra, not an extra percentage reserve", () => {
		const astra = {
			...model,
			id: "gpt-6-astra",
			provider: "openai-codex",
			api: "openai-codex-responses" as const,
			contextWindow: 272000,
		};
		expect(shouldCompactForModel(249999, astra, DEFAULT_COMPACTION_SETTINGS)).toBe(false);
		expect(shouldCompactForModel(250000, astra, DEFAULT_COMPACTION_SETTINGS)).toBe(true);
	});
	it("preserves disabled compaction and unknown-window handling", () => {
		expect(shouldCompact(999999, 1000000, { ...DEFAULT_COMPACTION_SETTINGS, enabled: false })).toBe(false);
		expect(shouldCompact(999999, 0, DEFAULT_COMPACTION_SETTINGS)).toBe(false);
	});
});

describe("empty-summary bounded recovery", () => {
	beforeEach(() => vi.mocked(completeSimple).mockReset());
	it("retries one empty response through the shared policy and preserves reasoning", async () => {
		vi.mocked(completeSimple).mockResolvedValueOnce(reply(" ")).mockResolvedValueOnce(reply("valid summary"));
		await expect(summarize()).resolves.toMatchObject({ summary: "valid summary" });
		expect(completeSimple).toHaveBeenCalledTimes(2);
		expect(vi.mocked(completeSimple).mock.calls[1][2]).toMatchObject({ reasoning: "max" });
	});
	it("fails loudly after the configured budget instead of saving an empty checkpoint", async () => {
		vi.mocked(completeSimple).mockResolvedValue(reply(""));
		await expect(summarize()).rejects.toThrow("Summarization returned an empty summary");
		expect(completeSimple).toHaveBeenCalledTimes(2);
	});
	it("does not reinterpret a cancelled response as retryable empty success", async () => {
		vi.mocked(completeSimple).mockResolvedValue(reply("", "aborted"));
		await expect(summarize()).rejects.toThrow("Summarization failed: aborted");
		expect(completeSimple).toHaveBeenCalledOnce();
	});
});
