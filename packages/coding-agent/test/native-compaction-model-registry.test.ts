import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { type NativeCompactionCapability, supportsCompaction } from "@earendil-works/pi-ai";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { AuthStorage } from "../src/core/auth-storage.js";
import { ModelRegistry, type ProviderConfigInput } from "../src/core/model-registry.js";

const provider = "azure-openai-managed";
const modelId = "gpt-6-astra";
const baseUrl = "http://127.0.0.1:43119/azure-openai/v1";

function capability(overrides: Partial<NativeCompactionCapability> = {}): NativeCompactionCapability {
	return {
		protocol: "openai-responses-compact-v1",
		provider,
		model: modelId,
		endpoint: `${baseUrl}/responses/compact`,
		apiVersion: "v1",
		enabled: false,
		validation: "unverified",
		...overrides,
	};
}

function config(nativeCompaction: unknown = undefined) {
	return {
		providers: {
			[provider]: {
				api: "openai-responses",
				apiKey: "TEST_LOCAL_TOKEN",
				baseUrl,
				models: [
					{
						id: modelId,
						name: "Managed Astra",
						reasoning: true,
						input: ["text", "image"],
						cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
						contextWindow: 1_050_000,
						maxTokens: 128_000,
						...(nativeCompaction === undefined ? {} : { nativeCompaction }),
					},
				],
			},
		},
	};
}

describe("ModelRegistry native compaction capability", () => {
	let tempDir: string;
	let modelsPath: string;
	let authPath: string;

	beforeEach(() => {
		tempDir = join(tmpdir(), `prime-native-compaction-model-${Date.now()}-${Math.random().toString(36).slice(2)}`);
		mkdirSync(tempDir, { recursive: true });
		modelsPath = join(tempDir, "models.json");
		authPath = join(tempDir, "auth.json");
	});

	afterEach(() => {
		if (existsSync(tempDir)) rmSync(tempDir, { recursive: true, force: true });
	});

	function load(value: unknown): ModelRegistry {
		writeFileSync(modelsPath, JSON.stringify(value), "utf8");
		return ModelRegistry.create(AuthStorage.create(authPath), modelsPath);
	}

	it("keeps absent and staged-unverified metadata disabled", () => {
		const absent = load(config());
		const absentModel = absent.find(provider, modelId);
		expect(absent.getError()).toBeUndefined();
		expect(absentModel?.nativeCompaction).toBeUndefined();
		expect(absentModel && supportsCompaction(absentModel)).toBe(false);

		const staged = load(config(capability()));
		const stagedModel = staged.find(provider, modelId);
		expect(staged.getError()).toBeUndefined();
		expect(stagedModel?.nativeCompaction).toEqual(capability());
		expect(stagedModel && supportsCompaction(stagedModel)).toBe(false);
	});

	it("round-trips only the exact live-verified allowlisted model", () => {
		const nativeCompaction = capability({ enabled: true, validation: "live-verified" });
		const registry = load(config(nativeCompaction));
		const model = registry.find(provider, modelId);
		expect(registry.getError()).toBeUndefined();
		expect(model?.nativeCompaction).toEqual(nativeCompaction);
		expect(model && supportsCompaction(model)).toBe(true);
	});

	it.each([
		capability({ enabled: true, validation: "unverified" }),
		capability({ enabled: true, validation: "documentation-verified" }),
		capability({ provider: "openai" }),
		capability({ model: "gpt-5.6-sol" }),
		capability({ endpoint: "http://127.0.0.1:43119/other/responses/compact" }),
		capability({ endpoint: `${baseUrl}/responses/compact?api-version=preview` }),
		capability({ endpoint: `http://user:password@127.0.0.1:43119/azure-openai/v1/responses/compact` }),
	])("rejects unsafe or mismatched capability metadata %#", (nativeCompaction) => {
		const registry = load(config(nativeCompaction));
		expect(registry.getError()).toMatch(/nativeCompaction/);
		expect(registry.find(provider, modelId)).toBeUndefined();
	});

	it("applies the same allowlist to dynamically registered providers", () => {
		const registry = load({ providers: {} });
		const dynamic: ProviderConfigInput = {
			api: "openai-responses",
			apiKey: "TEST_LOCAL_TOKEN",
			baseUrl,
			models: [
				{
					id: modelId,
					name: "Managed Astra",
					reasoning: true,
					input: ["text", "image"],
					cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
					contextWindow: 1_050_000,
					maxTokens: 128_000,
					nativeCompaction: capability({ enabled: true, validation: "live-verified" }),
				},
			],
		};
		expect(() => registry.registerProvider(provider, dynamic)).not.toThrow();
		expect(registry.find(provider, modelId)?.nativeCompaction?.enabled).toBe(true);
		expect(() =>
			registry.registerProvider("github-copilot", {
				...dynamic,
				models: dynamic.models?.map((model) => ({
					...model,
					nativeCompaction: capability({ provider: "github-copilot" }),
				})),
			}),
		).toThrow(/allowlisted only/);
	});

	it("refuses capability metadata on GitHub, Foundry, Ollama, and arbitrary Astra aliases", () => {
		for (const providerName of ["github-copilot", "azure-foundry", "ollama", "gateway"]) {
			const value = config(capability({ provider: providerName }));
			const providers: Record<string, unknown> = value.providers;
			const entry = providers[provider];
			delete providers[provider];
			providers[providerName] = entry;
			const registry = load(value);
			expect(registry.getError()).toMatch(/allowlisted only/);
		}
	});
});
