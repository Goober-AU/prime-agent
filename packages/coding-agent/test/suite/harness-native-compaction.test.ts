import type { AgentMessage } from "@earendil-works/pi-agent-core";
import {
	type CompactionOptions,
	type Context,
	fauxAssistantMessage,
	getApiProvider,
	type ProviderCompactionCheckpoint,
	registerApiProvider,
	unregisterApiProviders,
} from "@earendil-works/pi-ai";
import { afterEach, describe, expect, it } from "vitest";
import { convertResponsesMessages } from "../../../ai/src/providers/openai-responses-shared.js";
import { DEFAULT_COMPACTION_SETTINGS, estimateTokens, shouldCompact } from "../../src/core/compaction/compaction.js";
import {
	convertToLlm,
	createCompactionSummaryMessage,
	createHarnessDigestMessage,
	withoutHarnessDigestsForCompaction,
} from "../../src/core/messages.js";
import { SessionManager } from "../../src/core/session-manager.js";
import { createHarness, getMessageText, type Harness } from "./harness.js";

describe("static harness digest and fork-native compaction compatibility", () => {
	const harnesses: Harness[] = [];
	const registrations: string[] = [];
	afterEach(() => {
		while (harnesses.length) harnesses.pop()?.cleanup();
		while (registrations.length) unregisterApiProviders(registrations.pop()!);
	});

	it("keeps the full window and inclusive global 250K threshold", () => {
		expect(shouldCompact(249_999, 1_050_000, DEFAULT_COMPACTION_SETTINGS)).toBe(false);
		expect(shouldCompact(250_000, 1_050_000, DEFAULT_COMPACTION_SETTINGS)).toBe(true);
		const smallLimit = 128_000 - DEFAULT_COMPACTION_SETTINGS.reserveTokens;
		expect(shouldCompact(smallLimit - 1, 128_000, DEFAULT_COMPACTION_SETTINGS)).toBe(false);
		expect(shouldCompact(smallLimit, 128_000, DEFAULT_COMPACTION_SETTINGS)).toBe(true);
	});

	it("replays opaque items and the mechanical digest as separate Responses input", async () => {
		const harness = await createHarness();
		harnesses.push(harness);
		const model = harness.getModel();
		const checkpoint: ProviderCompactionCheckpoint = {
			version: 1,
			provider: model.provider,
			api: model.api,
			model: model.id,
			baseUrl: model.baseUrl,
			items: [{ type: "compaction", encrypted_content: "opaque-checkpoint-fixture" }],
			estimatedTokens: 500,
		};
		const digest = "Exact persistent memory; not a generated summary.";
		const head = createCompactionSummaryMessage(
			"display-only summary",
			250_000,
			new Date().toISOString(),
			undefined,
			0,
			checkpoint,
			digest,
		);
		const before = JSON.stringify(checkpoint);
		const llm = convertToLlm([head]);
		expect(llm).toHaveLength(2);
		expect(llm[0]).toMatchObject({ role: "user", providerContext: checkpoint });
		expect(getMessageText(llm[1])).toContain(digest);
		const wire = convertResponsesMessages(model, { messages: llm }, new Set());
		expect(wire[0]).toEqual(checkpoint.items[0]);
		expect(wire[1]).toMatchObject({
			role: "user",
			content: [{ type: "input_text", text: expect.stringContaining(digest) }],
		});
		expect(JSON.stringify(wire)).not.toContain("display-only summary");
		expect(JSON.stringify(checkpoint)).toBe(before);
		expect(estimateTokens(head)).toBe(500 + Math.ceil(digest.length / 4));

		const source: AgentMessage[] = [
			head,
			createHarnessDigestMessage("new digest"),
			{ role: "user", content: "real work", timestamp: Date.now() },
		];
		const forCompaction = withoutHarnessDigestsForCompaction(source);
		expect(forCompaction).toHaveLength(2);
		expect(forCompaction[0]).toMatchObject({ providerContext: checkpoint, harnessDigest: undefined });
		expect(head.harnessDigest).toBe(digest);
		expect(JSON.stringify(convertToLlm(forCompaction))).not.toContain(digest);
		expect(JSON.stringify(convertToLlm(forCompaction))).toContain("real work");
	});

	it("persists the digest outside two native compactions, keeps context hooks and a 20-minute request timeout", async () => {
		let hookCalls = 0;
		const harness = await createHarness({
			api: "faux-native-digest",
			models: [{ id: "gpt-6-astra-fixture", contextWindow: 1_050_000 }],
			settings: { compaction: { keepRecentTokens: 1 } },
			persistSession: true,
			extensionFactories: [
				(pi) => {
					pi.on("context", async (event) => {
						hookCalls++;
						return { messages: event.messages.filter((message) => getMessageText(message) !== "redact-me") };
					});
				},
			],
		});
		harnesses.push(harness);
		harness.setResponses([
			fauxAssistantMessage("one response"),
			fauxAssistantMessage("two response"),
			fauxAssistantMessage("following command succeeded"),
		]);
		await harness.session.prompt("one");
		await harness.session.prompt("two");
		const model = harness.getModel();
		const provider = getApiProvider(model.api)!;
		const sourceId = "harness-native-compaction-fixture";
		registrations.push(sourceId);
		const calls: { context: Context; options?: CompactionOptions }[] = [];
		registerApiProvider(
			{
				...provider,
				supportsCompaction: () => true,
				compact: async (_model, context, options) => {
					calls.push({ context, options });
					return {
						checkpoint: {
							version: 1,
							provider: model.provider,
							api: model.api,
							model: model.id,
							baseUrl: model.baseUrl,
							items: [{ type: "compaction", encrypted_content: `opaque-fixture-${calls.length}` }],
							estimatedTokens: 400,
						},
					};
				},
			},
			sourceId,
		);
		harness.session.agent.state.messages.push({ role: "user", content: "redact-me", timestamp: Date.now() });
		const beforeHooks = hookCalls;
		await harness.session.compact();
		expect(hookCalls).toBeGreaterThan(beforeHooks);
		expect(calls).toHaveLength(1);
		expect(calls[0]?.options?.timeoutMs).toBe(1_200_000);
		expect(calls[0]?.context.systemPrompt).toBe(harness.session.agent.state.systemPrompt);
		expect(JSON.stringify(calls[0]?.context.messages)).not.toContain("redact-me");
		expect(JSON.stringify(calls[0]?.context.messages)).not.toContain("<harness_state>");
		expect(model.contextWindow).toBe(1_050_000);
		const head = harness.session.messages[0];
		expect(head).toMatchObject({
			role: "compactionSummary",
			providerContext: { estimatedTokens: 400 },
			harnessDigest: expect.any(String),
		});

		const reopened = SessionManager.open(harness.sessionManager.getSessionFile()!).buildSessionContext(
			model,
		).messages;
		expect(reopened[0]).toEqual(head);
		expect(convertToLlm(reopened).some((message) => getMessageText(message).includes("<harness_state>"))).toBe(true);
		await harness.session.prompt("after native compaction");
		expect(getMessageText(harness.session.messages.at(-1))).toBe("following command succeeded");
		await harness.session.compact();
		expect(calls).toHaveLength(2);
		expect(JSON.stringify(calls[1]?.context.messages)).not.toContain("<harness_state>");
		expect(calls[1]?.context.messages[0]).toMatchObject({
			providerContext: { items: [{ encrypted_content: "opaque-fixture-1" }] },
		});
		expect(harness.session.messages[0]).toMatchObject({
			providerContext: { items: [{ encrypted_content: "opaque-fixture-2" }] },
			harnessDigest: expect.any(String),
		});
	});
});
