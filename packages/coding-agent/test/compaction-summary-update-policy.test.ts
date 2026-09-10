import type { AgentMessage } from "@earendil-works/pi-agent-core";
import type * as ai from "@earendil-works/pi-ai";
import type { Api, AssistantMessage, Model, Usage } from "@earendil-works/pi-ai";
import { completeSimple } from "@earendil-works/pi-ai";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
	buildSummarizationPrompt,
	CONSOLIDATE_REPEATED_SUMMARY_POLICY,
	type CompactionPreparation,
	compact,
	createFileOps,
	generateSummary,
	resolveSummaryUpdatePolicy,
} from "../src/core/compaction/index.js";

vi.mock("@earendil-works/pi-ai", async (importOriginal) => ({
	...(await importOriginal<typeof ai>()),
	completeSimple: vi.fn(),
}));

const model: Model<Api> = {
	id: "summary-policy-model",
	name: "Summary policy model",
	provider: "test-provider",
	api: "anthropic-messages",
	baseUrl: "https://example.invalid",
	reasoning: true,
	input: ["text"],
	contextWindow: 200_000,
	maxTokens: 16_384,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
};

const usage = (): Usage => ({
	input: 10,
	output: 5,
	cacheRead: 0,
	cacheWrite: 0,
	totalTokens: 15,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
});

function reply(text: string, stopReason: AssistantMessage["stopReason"] = "stop"): AssistantMessage {
	return {
		role: "assistant",
		content: text ? [{ type: "text", text }] : [],
		api: model.api,
		provider: model.provider,
		model: model.id,
		usage: usage(),
		stopReason,
		timestamp: Date.now(),
	};
}

function user(content: string, timestamp = Date.now()): AgentMessage {
	return { role: "user", content, timestamp };
}

function requestText(callIndex: number): string {
	const context = vi.mocked(completeSimple).mock.calls[callIndex][1];
	const message = context.messages[0];
	if (message.role !== "user" || typeof message.content !== "string") throw new Error("expected summary request text");
	return message.content;
}

async function update(messages: AgentMessage[], previousSummary: string, signal?: AbortSignal) {
	return generateSummary(
		messages,
		model,
		8_000,
		"test-only",
		undefined,
		signal,
		undefined,
		previousSummary,
		"max",
		undefined,
		undefined,
		CONSOLIDATE_REPEATED_SUMMARY_POLICY,
	);
}

describe("opt-in ordinary-summary consolidation policy", () => {
	beforeEach(() => {
		vi.mocked(completeSimple).mockReset();
	});

	it("is default-off and leaves the legacy update prompt unchanged", () => {
		expect(resolveSummaryUpdatePolicy(undefined, undefined)).toBe("off");
		expect(resolveSummaryUpdatePolicy("unknown", "unknown")).toBe("off");
		expect(resolveSummaryUpdatePolicy("off", CONSOLIDATE_REPEATED_SUMMARY_POLICY)).toBe("off");
		const implicit = buildSummarizationPrompt(undefined, "old summary");
		const explicitOff = buildSummarizationPrompt(undefined, "old summary", "off");
		expect(explicitOff).toBe(implicit);
		expect(implicit).toContain("If something is no longer relevant, you may remove it");
		expect(implicit).not.toContain("Do not apply a character/token cap");
	});

	it("adds only a prompt policy and preserves exact constraint, reason, blocker, artifact, error, kernel, and harness rules", () => {
		const prompt = buildSummarizationPrompt(
			"Keep the exact user sentence verbatim.",
			"prior",
			CONSOLIDATE_REPEATED_SUMMARY_POLICY,
		);
		for (const anchor of [
			"ONLY the NEW conversation material",
			"Do not make another merge pass",
			"user constraint",
			"correction",
			"reasons and provenance",
			"unresolved work and blockers",
			"paths, IDs, hashes, sizes, commands, function names, critical errors",
			"kernel-state and continual-harness facts",
			"tool-call/result relationships",
			"opaque provider checkpoint",
			"When uncertain",
			"Do not apply a character/token cap, tail truncation, or arbitrary deletion",
			"Keep the exact user sentence verbatim.",
		]) {
			expect(prompt).toContain(anchor);
		}
	});

	it("chains five repeated updates with one same-model max-reasoning call each and no duplicate segment ledger", async () => {
		const stable = [
			"## Goal",
			"Ship fixture",
			"## Constraints & Preferences",
			"- Never remove ERROR_EXACT",
			"## Key Decisions",
			"- Keep C:/evidence/a.json because decision D1",
		].join("\n");
		vi.mocked(completeSimple).mockResolvedValue(reply(stable));
		let previous = stable;
		for (let index = 0; index < 5; index++) {
			const marker = `REPEATED-UNCHANGED-${index}`;
			previous = (await update([user(`Still unchanged. ${marker}`)], previous)).summary;
			const wire = requestText(index);
			expect(wire).toContain(`<previous-summary>\n${stable}\n</previous-summary>`);
			expect(wire).toContain(marker);
			for (let older = 0; older < index; older++) expect(wire).not.toContain(`REPEATED-UNCHANGED-${older}`);
			expect(vi.mocked(completeSimple).mock.calls[index][0]).toBe(model);
			expect(vi.mocked(completeSimple).mock.calls[index][2]).toMatchObject({ reasoning: "max" });
		}
		expect(previous).toBe(stable);
		expect(completeSimple).toHaveBeenCalledTimes(5);
	});

	it("chains five genuinely new facts without a cap or tail truncation", async () => {
		const facts = [
			"CONSTRAINT: preserve exact wording X",
			"DECISION D2 because reason R2",
			"BLOCKER B1 remains open",
			"ARTIFACT C:/evidence/b.json sha256=abc size=123",
			"KERNEL helper_name and HARNESS memory-id remain available",
		];
		let expected = "## Goal\nInitial";
		vi.mocked(completeSimple).mockImplementation(async (_model, context) => {
			const request = context.messages[0];
			if (request.role !== "user" || typeof request.content !== "string") throw new Error("bad fixture");
			const requestContent = request.content;
			const previousSummaryStart = requestContent.indexOf("<previous-summary>");
			const newMaterial = previousSummaryStart >= 0 ? requestContent.slice(0, previousSummaryStart) : requestContent;
			const fact = facts.find((candidate) => newMaterial.includes(candidate));
			if (!fact) throw new Error("missing new fact");
			const previous = requestContent.match(/<previous-summary>\n([\s\S]*?)\n<\/previous-summary>/)?.[1];
			return reply(`${previous}\n- ${fact}`);
		});

		for (let index = 0; index < facts.length; index++) {
			expected += `\n- ${facts[index]}`;
			const result = await update(
				[user(facts[index])],
				index === 0 ? "## Goal\nInitial" : expected.slice(0, -`\n- ${facts[index]}`.length),
			);
			expect(result.summary).toBe(expected);
			for (let seen = 0; seen <= index; seen++) expect(result.summary).toContain(facts[seen]);
			const wire = requestText(index);
			expect(wire).toContain(facts[index]);
			for (let future = index + 1; future < facts.length; future++) expect(wire).not.toContain(facts[future]);
		}
		expect(completeSimple).toHaveBeenCalledTimes(5);
	});

	it("makes supersession evidence explicit instead of authorizing arbitrary deletion", async () => {
		const previous = [
			"## Progress",
			"### Blocked",
			"- Upload blocked by ERROR_AUTH at C:/logs/failure.txt",
			"## Key Decisions",
			"- Use route A because approval P1",
		].join("\n");
		const fixture = [
			"## Progress",
			"### Done",
			"- Upload completed after token repair; ERROR_AUTH resolved by receipt R9",
			"### Blocked",
			"- (none)",
			"## Key Decisions",
			"- Use route A because approval P1",
			"## Critical Context",
			"- Prior ERROR_AUTH at C:/logs/failure.txt was resolved by receipt R9",
		].join("\n");
		vi.mocked(completeSimple).mockResolvedValue(reply(fixture));
		const result = await update([user("Receipt R9 proves upload succeeded and resolves ERROR_AUTH.")], previous);
		expect(result.summary).toBe(fixture);
		expect(requestText(0)).toContain(previous);
		expect(requestText(0)).toContain("Receipt R9 proves");
		expect(requestText(0)).toContain(
			"Mark a blocker resolved or replace an old status only when the new messages demonstrate",
		);
	});

	it("keeps existing split-turn mechanics at exactly two calls", async () => {
		vi.mocked(completeSimple)
			.mockResolvedValueOnce(reply("updated history"))
			.mockResolvedValueOnce(reply("turn prefix"));
		const preparation: CompactionPreparation = {
			firstKeptEntryId: "kept-1",
			messagesToSummarize: [user("old history only", 1)],
			turnPrefixMessages: [user("prefix only", 2)],
			isSplitTurn: true,
			tokensBefore: 1234,
			previousSummary: "previous summary",
			fileOps: createFileOps(),
			settings: {
				enabled: true,
				reserveTokens: 8_000,
				keepRecentTokens: 1,
				summaryUpdatePolicy: CONSOLIDATE_REPEATED_SUMMARY_POLICY,
			},
		};
		const result = await compact(preparation, model, "test-only", undefined, undefined, undefined, "max");
		expect(completeSimple).toHaveBeenCalledTimes(2);
		expect(result.summary).toContain("updated history");
		expect(result.summary).toContain("turn prefix");
		const requests = [requestText(0), requestText(1)];
		expect(requests.some((request) => request.includes("ONLY the NEW conversation material"))).toBe(true);
		expect(requests.some((request) => request.includes("PREFIX of a turn"))).toBe(true);
		for (const call of vi.mocked(completeSimple).mock.calls) {
			expect(call[0]).toBe(model);
			expect(call[2]).toMatchObject({ reasoning: "max" });
		}
	});

	it("keeps branch material and the prior summary through a model switch without a side ledger", async () => {
		const switchedModel: Model<Api> = {
			...model,
			id: "summary-policy-model-switched",
			name: "Summary policy model switched",
		};
		vi.mocked(completeSimple).mockResolvedValue(reply("branch-aware update"));
		await generateSummary(
			[
				{
					role: "branchSummary",
					summary: "Branch decision B7 because reason R7",
					fromId: "entry-branch-7",
					timestamp: 7,
				},
				user("New material after reload", 8),
			],
			switchedModel,
			8_000,
			"test-only",
			undefined,
			undefined,
			undefined,
			"Previous exact constraint C7",
			"max",
			undefined,
			undefined,
			CONSOLIDATE_REPEATED_SUMMARY_POLICY,
		);
		expect(completeSimple).toHaveBeenCalledOnce();
		expect(vi.mocked(completeSimple).mock.calls[0][0]).toBe(switchedModel);
		expect(vi.mocked(completeSimple).mock.calls[0][2]).toMatchObject({ reasoning: "max" });
		const wire = requestText(0);
		expect(wire).toContain("Previous exact constraint C7");
		expect(wire).toContain("Branch decision B7 because reason R7");
		expect(wire).toContain("New material after reload");
	});

	it("keeps a tool call and its result paired in the one update request", async () => {
		vi.mocked(completeSimple).mockResolvedValue(reply("tool-aware update"));
		const assistant: AssistantMessage = {
			role: "assistant",
			content: [{ type: "toolCall", id: "tool-call-9", name: "ipython", arguments: { code: "verify()" } }],
			api: model.api,
			provider: model.provider,
			model: model.id,
			usage: usage(),
			stopReason: "toolUse",
			timestamp: 9,
		};
		await update(
			[
				assistant,
				{
					role: "toolResult",
					toolCallId: "tool-call-9",
					toolName: "ipython",
					content: [{ type: "text", text: "VERIFIED_RESULT_9" }],
					isError: false,
					timestamp: 10,
				},
			],
			"Previous summary",
		);
		expect(completeSimple).toHaveBeenCalledOnce();
		const wire = requestText(0);
		expect(wire.indexOf('ipython(code="verify()")')).toBeLessThan(wire.indexOf("VERIFIED_RESULT_9"));
		expect(wire).toContain("[Assistant tool calls]");
		expect(wire).toContain("[Tool result]");
	});

	it("honors pre-cancellation without a model pass", async () => {
		const controller = new AbortController();
		controller.abort(new Error("cancel summary"));
		await expect(update([user("new material")], "previous", controller.signal)).rejects.toThrow();
		expect(completeSimple).not.toHaveBeenCalled();
	});
});
