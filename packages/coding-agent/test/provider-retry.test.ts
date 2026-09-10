import type { AssistantMessage } from "@earendil-works/pi-ai";
import { describe, expect, it, vi } from "vitest";
import { completeWithProviderRetry, providerRetryDelay, requestWithProviderRetry } from "../src/core/provider-retry.js";

function providerError(kind?: string, retryAfterMs?: number): AssistantMessage {
	return {
		role: "assistant",
		content: [],
		api: "openai-completions",
		provider: "openai",
		model: "test-model",
		usage: {
			input: 0,
			output: 0,
			cacheRead: 0,
			cacheWrite: 0,
			totalTokens: 0,
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		},
		stopReason: "error",
		errorMessage: "500 Internal Server Error",
		timestamp: Date.now(),
		...(kind
			? {
					diagnostics: [
						{
							type: "provider_stream_failure" as const,
							timestamp: Date.now(),
							details: { kind, ...(retryAfterMs === undefined ? {} : { retryAfterMs }) },
						},
					],
				}
			: {}),
	};
}

const policy = { enabled: true, maxRetries: 3, baseDelayMs: 2_000, maxRetryDelayMs: 60_000 };

describe("providerRetryDelay", () => {
	it("applies bounded +/-20% jitter to client backoff", () => {
		expect(providerRetryDelay(1, undefined, policy, { random: () => 0 })).toEqual({
			kind: "wait",
			delayMs: 1_600,
		});
		expect(providerRetryDelay(1, undefined, policy, { random: () => 1 })).toEqual({
			kind: "wait",
			delayMs: 2_400,
		});
	});

	it("treats Retry-After as a not-before floor and adds only a positive client offset", () => {
		expect(providerRetryDelay(1, 30_000, policy, { random: () => 0 })).toEqual({
			kind: "wait",
			delayMs: 30_000,
		});
		expect(providerRetryDelay(1, 30_000, policy, { random: () => 1 })).toEqual({
			kind: "wait",
			delayMs: 30_400,
		});
	});

	it("spreads simultaneous children without changing attempt ownership", () => {
		const retryTimes = Array.from({ length: 101 }, (_, index) =>
			providerRetryDelay(1, 10_000, policy, { random: () => index / 100 }),
		)
			.filter((delay): delay is { kind: "wait"; delayMs: number } => delay.kind === "wait")
			.map((delay) => delay.delayMs);
		expect(new Set(retryTimes).size).toBeGreaterThan(90);
		expect(Math.min(...retryTimes)).toBe(10_000);
		expect(Math.max(...retryTimes)).toBe(10_400);
	});

	it("sanitizes non-finite random, jitter, and base-delay inputs", () => {
		expect(
			providerRetryDelay(
				1,
				undefined,
				{ baseDelayMs: Number.NaN, maxRetryDelayMs: 60_000 },
				{
					random: () => Number.POSITIVE_INFINITY,
					jitterRatio: Number.NaN,
				},
			),
		).toEqual({ kind: "wait", delayMs: 2_000 });
		expect(
			providerRetryDelay(
				1,
				undefined,
				{ baseDelayMs: Number.POSITIVE_INFINITY, maxRetryDelayMs: 60_000 },
				{
					random: () => Number.NaN,
					jitterRatio: Number.POSITIVE_INFINITY,
				},
			),
		).toEqual({ kind: "wait", delayMs: 2_000 });
	});

	it("fails rather than overflowing a timer and retrying before a long server delay", () => {
		const ninetyDaysMs = 90 * 24 * 3600 * 1000;
		expect(providerRetryDelay(1, ninetyDaysMs, { baseDelayMs: 2_000, maxRetryDelayMs: 0 })).toEqual({
			kind: "exceeds-cap",
			retryAfterMs: ninetyDaysMs,
		});
	});
});

describe("completeWithProviderRetry", () => {
	it("returns an aborted result when cancelled during the injected backoff", async () => {
		const controller = new AbortController();
		const sleeper = vi.fn(async (_delayMs: number, signal?: AbortSignal) => {
			controller.abort();
			if (signal?.aborted) throw new Error("Aborted");
		});

		const result = await completeWithProviderRetry(async () => providerError(), {
			policy,
			signal: controller.signal,
			sleep: sleeper,
			random: () => 0.5,
		});

		expect(sleeper).toHaveBeenCalledOnce();
		expect(result.stopReason).toBe("aborted");
	});

	it("makes a single attempt when the policy disables retries", async () => {
		let attempts = 0;
		const result = await completeWithProviderRetry(
			async () => {
				attempts++;
				return providerError();
			},
			{ policy: { enabled: false, maxRetries: 3, baseDelayMs: 1, maxRetryDelayMs: 60_000 } },
		);

		expect(attempts).toBe(1);
		expect(result.stopReason).toBe("error");
	});

	it.each(["invalid_request", "permission", "refusal"])("does not retry permanent %s failures", async (kind) => {
		let attempts = 0;
		const result = await completeWithProviderRetry(
			async () => {
				attempts++;
				return providerError(kind);
			},
			{ policy, sleep: async () => undefined },
		);
		expect(attempts).toBe(1);
		expect(result.stopReason).toBe("error");
	});

	it("does not start a retry that cannot finish waiting before the deadline", async () => {
		let attempts = 0;
		const sleeper = vi.fn(async () => undefined);
		const result = await completeWithProviderRetry(
			async () => {
				attempts++;
				return providerError("rate_limit", 2_000);
			},
			{ policy, deadlineAtMs: 2_999, now: () => 1_000, random: () => 0, sleep: sleeper },
		);
		expect(attempts).toBe(1);
		expect(sleeper).not.toHaveBeenCalled();
		expect(result.stopReason).toBe("error");
	});

	it("does not dispatch after an injected sleep overshoots the deadline", async () => {
		let now = 1_000;
		let attempts = 0;
		const firstError = providerError("rate_limit");
		const result = await completeWithProviderRetry(
			async () => {
				attempts++;
				return firstError;
			},
			{
				policy,
				deadlineAtMs: 3_000,
				now: () => now,
				random: () => 0,
				sleep: async () => {
					now = 3_001;
				},
			},
		);
		expect(attempts).toBe(1);
		expect(result).toBe(firstError);
	});

	it("never retries a successful response after visible provider work", async () => {
		let attempts = 0;
		const success = {
			...providerError(),
			stopReason: "stop" as const,
			content: [{ type: "text" as const, text: "done" }],
		};
		const result = await completeWithProviderRetry(async () => {
			attempts++;
			return success;
		});
		expect(attempts).toBe(1);
		expect(result).toBe(success);
	});
});

describe("requestWithProviderRetry", () => {
	it.each([400, 401, 403, 422])("does not retry non-transient HTTP %s", async (status) => {
		const request = vi.fn(async () => {
			throw Object.assign(new Error("rejected"), { status });
		});
		await expect(requestWithProviderRetry(request, { policy, sleep: async () => undefined })).rejects.toThrow(
			"rejected",
		);
		expect(request).toHaveBeenCalledOnce();
	});

	it("keeps the original failure when sleep overshoots the deadline", async () => {
		let now = 1_000;
		const error = Object.assign(new Error("busy"), { status: 503 });
		const request = vi.fn(async () => {
			throw error;
		});
		await expect(
			requestWithProviderRetry(request, {
				policy,
				deadlineAtMs: 3_000,
				now: () => now,
				random: () => 0,
				sleep: async () => {
					now = 3_001;
				},
			}),
		).rejects.toBe(error);
		expect(request).toHaveBeenCalledOnce();
	});

	it("honors the injected clock and deadline for thrown rate limits", async () => {
		const error = Object.assign(new Error("busy"), { status: 429, retryAfterMs: 5_000 });
		const request = vi.fn(async () => {
			throw error;
		});
		const sleeper = vi.fn(async () => undefined);
		await expect(
			requestWithProviderRetry(request, {
				policy,
				deadlineAtMs: 5_999,
				now: () => 1_000,
				random: () => 0,
				sleep: sleeper,
			}),
		).rejects.toBe(error);
		expect(request).toHaveBeenCalledOnce();
		expect(sleeper).not.toHaveBeenCalled();
	});
});
