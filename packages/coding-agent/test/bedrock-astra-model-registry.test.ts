import { getModelInputLimit, getSupportedThinkingLevels, supportsCompaction } from "@earendil-works/pi-ai";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AuthStorage } from "../src/core/auth-storage.js";
import { ModelRegistry } from "../src/core/model-registry.js";

afterEach(() => vi.unstubAllEnvs());

describe("Bedrock Astra in the shared session model catalog", () => {
	it.each(["api-key", "profile"])("makes both regional routes available with %s authentication", async (mode) => {
		vi.stubEnv("PI_OFFLINE", "1");
		vi.stubEnv("AWS_BEARER_TOKEN_BEDROCK", "");
		vi.stubEnv("AWS_PROFILE", mode === "profile" ? "test-profile" : "");
		const auth = AuthStorage.inMemory(
			mode === "api-key" ? { "amazon-bedrock": { type: "api_key", key: "test-bedrock-token" } } : {},
		);
		const registry = ModelRegistry.inMemory(auth);
		const models = registry.getAvailable().filter((model) => model.api === "bedrock-responses");
		expect(models.map((model) => model.id).sort()).toEqual(["global.openai.gpt-6-astra", "openai.gpt-6-astra"]);
		for (const model of models) {
			expect(registry.find("amazon-bedrock", model.id)).toEqual(model);
			expect(getSupportedThinkingLevels(model)).toEqual(["low", "medium", "high", "xhigh", "max"]);
			expect(getModelInputLimit(model)).toBe(922_000);
			expect(supportsCompaction(model)).toBe(false);
			expect(await registry.getApiKeyAndHeaders(model)).toMatchObject({
				ok: true,
				apiKey: mode === "api-key" ? "test-bedrock-token" : "<authenticated>",
			});
		}
		expect(registry.getAvailable().some((model) => model.api === "bedrock-converse-stream")).toBe(true);
	});
});
