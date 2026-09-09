import { compactionMatchesModel, getModel } from "@earendil-works/pi-ai";
import { describe, expect, test } from "vitest";
import { getProviderCheckpoint, hasProviderCheckpoint } from "../src/core/compaction/checkpoint.js";
import { buildSessionContext, type SessionEntry } from "../src/core/session-manager.js";

const model = { ...getModel("openai-codex", "gpt-6-astra"), baseUrl: "https://chatgpt.com/backend-api" };
const legacy = {
	strategy: "openai-responses-compaction-v2",
	provider: model.provider,
	api: model.api,
	model: model.id,
	baseUrl: model.baseUrl,
	compactedWindow: [
		{ type: "message", role: "user", content: [{ type: "input_text", text: "retained user" }] },
		{ type: "compaction", encrypted_content: "synthetic-opaque-checkpoint" },
	],
};
const timestamp = "2026-09-09T00:00:00.000Z";

function entries(details: unknown): SessionEntry[] {
	return [
		{
			type: "message",
			id: "a",
			parentId: null,
			timestamp,
			message: { role: "user", content: "Original fallback history", timestamp: 1 },
		},
		{
			type: "compaction",
			id: "b",
			parentId: "a",
			timestamp,
			summary: "[OpenAI native compaction checkpoint]",
			firstKeptEntryId: "a",
			tokensBefore: 250000,
			details,
		},
		{
			type: "message",
			id: "c",
			parentId: "b",
			timestamp,
			message: { role: "user", content: "Continue new work", timestamp: 2 },
		},
	];
}

describe("legacy provider checkpoint read compatibility", () => {
	test("preserves opaque items and endpoint identity without mutating old details", () => {
		const before = structuredClone(legacy);
		expect(hasProviderCheckpoint(legacy)).toBe(true);
		const checkpoint = getProviderCheckpoint(legacy);
		expect(checkpoint?.items).toEqual(legacy.compactedWindow);
		expect(checkpoint?.estimatedTokens).toBeGreaterThan(0);
		expect(checkpoint && compactionMatchesModel(checkpoint, model)).toBe(true);
		expect(legacy).toEqual(before);
	});

	test("uses the current schema preferentially and rejects an invalid current schema", () => {
		const checkpoint = getProviderCheckpoint(legacy);
		expect(getProviderCheckpoint({ providerCheckpoint: checkpoint })).toEqual(checkpoint);
		expect(getProviderCheckpoint({ ...legacy, providerCheckpoint: { version: 999 } })).toBeUndefined();
	});

	test.each([
		null,
		{},
		{ ...legacy, strategy: "unknown-checkpoint" },
		{ ...legacy, compactedWindow: [] },
		{ ...legacy, compactedWindow: [{ type: "compaction", encrypted_content: "" }] },
		{ ...legacy, compactedWindow: [null, ...legacy.compactedWindow] },
		{ ...legacy, provider: undefined },
		{ ...legacy, api: undefined },
		{ ...legacy, model: undefined },
		{ ...legacy, baseUrl: undefined },
	])("rejects malformed or unrecognized legacy data %#", (details) => {
		expect(getProviderCheckpoint(details)).toBeUndefined();
	});

	test("counts embedded images without charging base64 length as text", () => {
		const compactedWindow = [
			...legacy.compactedWindow,
			{
				type: "message",
				content: [{ type: "input_image", image_url: `data:image/png;base64,${"A".repeat(10000)}` }],
			},
		];
		const checkpoint = getProviderCheckpoint({ ...legacy, compactedWindow });
		expect(checkpoint?.estimatedTokens).toBeGreaterThan(1200);
		expect(checkpoint?.estimatedTokens).toBeLessThan(2000);
		expect(checkpoint?.items).toEqual(compactedWindow);
	});

	test("replays the legacy checkpoint and retains the next prompt", () => {
		const context = JSON.stringify(buildSessionContext(entries(legacy), "c", undefined, model).messages);
		expect(context).toContain("synthetic-opaque-checkpoint");
		expect(context).toContain("Continue new work");
		expect(context).not.toContain("Original fallback history");
	});

	test.each([
		{ ...model, provider: "github-copilot" },
		{ ...model, id: "gpt-5.6-sol" },
		{ ...model, baseUrl: "https://different.example.invalid" },
	])("reconstructs original history instead of replaying to a different target %#", (target) => {
		const context = JSON.stringify(buildSessionContext(entries(legacy), "c", undefined, target).messages);
		expect(context).toContain("Original fallback history");
		expect(context).not.toContain("synthetic-opaque-checkpoint");
	});
});
