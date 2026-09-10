import type { AssistantMessage } from "@earendil-works/pi-ai";
import { sleep } from "../utils/sleep.js";
import type { SettingsManager } from "./settings-manager.js";

/**
 * The single retry policy (permanent kinds, Retry-After-aware capped delays),
 * shared by the AgentSession auto-retry loop and the one-shot completion
 * consumers (side questions, compaction, refinement, session summaries).
 */
export interface ProviderRetryPolicy {
	enabled: boolean;
	maxRetries: number;
	baseDelayMs: number;
	/** Max server-requested retry delay before giving up; 0 disables the cap. */
	maxRetryDelayMs: number;
}

export function providerRetryPolicy(settingsManager: SettingsManager): ProviderRetryPolicy {
	return {
		...settingsManager.getRetrySettings(),
		maxRetryDelayMs: settingsManager.getProviderRetrySettings().maxRetryDelayMs,
	};
}

/** Local listener/lifecycle crashes are not provider failures; never retry them. */
export function isAgentLifecycleFailure(message: AssistantMessage): boolean {
	return message.diagnostics?.some((diagnostic) => diagnostic.type === "agent_lifecycle_failure") ?? false;
}

/** The faux test provider's queue running dry is deterministic; retrying it only stalls tests. */
export function isFauxProviderQueueExhausted(message: AssistantMessage): boolean {
	return message.provider === "faux" && message.errorMessage === "No more faux responses queued";
}

export function providerStreamFailureDetails(message: AssistantMessage): Record<string, unknown> | undefined {
	const failure = message.diagnostics?.find((diagnostic) => diagnostic.type === "provider_stream_failure");
	const details = failure?.details;
	if (!details || typeof details !== "object") {
		return undefined;
	}
	return details;
}

export function providerStreamFailureKind(message: AssistantMessage): string | undefined {
	const kind = providerStreamFailureDetails(message)?.kind;
	return typeof kind === "string" ? kind : undefined;
}

export function providerStreamFailureRetryAfterMs(message: AssistantMessage): number | undefined {
	const value = providerStreamFailureDetails(message)?.retryAfterMs;
	return typeof value === "number" && value >= 0 ? value : undefined;
}

/** Deterministic rejections never retry; auth gets one retry before it can be marked stale. */
export function isPermanentProviderFailureKind(kind: string | undefined, retriesPerformed: number): boolean {
	if (kind === "invalid_request" || kind === "refusal" || kind === "permission") {
		return true;
	}
	return retriesPerformed > 0 && kind === "auth";
}

export type ProviderRetryDelay = { kind: "wait"; delayMs: number } | { kind: "exceeds-cap"; retryAfterMs: number };

export interface ProviderRetryDelayOptions {
	/** Uniform source in [0, 1]. Injected by deterministic tests. */
	random?: () => number;
	/** Fractional client-backoff jitter. */
	jitterRatio?: number;
}

export interface ProviderRetryExecutionOptions extends ProviderRetryDelayOptions {
	policy?: ProviderRetryPolicy;
	signal?: AbortSignal;
	/** Absolute deadline in the same millisecond timebase returned by now(). */
	deadlineAtMs?: number;
	now?: () => number;
	sleep?: (delayMs: number, signal?: AbortSignal) => Promise<void>;
}

/** Node caps timers at 2^31-1 ms; longer delays overflow setTimeout and fire after ~1ms. */
const MAX_TIMER_DELAY_MS = 2_147_483_647;
const DEFAULT_PROVIDER_RETRY_BASE_DELAY_MS = 2_000;
export const PROVIDER_RETRY_JITTER_RATIO = 0.2;

function boundedRandom(source: () => number): number {
	const value = source();
	if (!Number.isFinite(value)) return 0.5;
	return Math.min(1, Math.max(0, value));
}

/** Delay before retry `attempt` (1-based), honoring a server-requested not-before time. */
export function providerRetryDelay(
	attempt: number,
	retryAfterMs: number | undefined,
	policy: Pick<ProviderRetryPolicy, "baseDelayMs" | "maxRetryDelayMs">,
	options: ProviderRetryDelayOptions = {},
): ProviderRetryDelay {
	if (retryAfterMs !== undefined && policy.maxRetryDelayMs > 0 && retryAfterMs > policy.maxRetryDelayMs) {
		return { kind: "exceeds-cap", retryAfterMs };
	}
	// A timer longer than Node's supported range would fire immediately. Failing
	// preserves Retry-After's not-before contract instead of replaying early.
	if (retryAfterMs !== undefined && retryAfterMs > MAX_TIMER_DELAY_MS) {
		return { kind: "exceeds-cap", retryAfterMs };
	}
	const random = boundedRandom(options.random ?? Math.random);
	const requestedJitterRatio = options.jitterRatio ?? PROVIDER_RETRY_JITTER_RATIO;
	const jitterRatio = Number.isFinite(requestedJitterRatio)
		? Math.min(1, Math.max(0, requestedJitterRatio))
		: PROVIDER_RETRY_JITTER_RATIO;
	const requestedBaseDelayMs = policy.baseDelayMs;
	const baseDelayMs = Number.isFinite(requestedBaseDelayMs)
		? Math.max(0, requestedBaseDelayMs)
		: DEFAULT_PROVIDER_RETRY_BASE_DELAY_MS;
	const exponential = Math.min(baseDelayMs * 2 ** Math.max(0, attempt - 1), MAX_TIMER_DELAY_MS);
	let delayMs: number;
	if (retryAfterMs !== undefined && retryAfterMs >= exponential) {
		// Retry-After is a floor, not a client delay to scale down. Add only a
		// bounded positive client offset so peers given the same floor still spread.
		delayMs = retryAfterMs + Math.round(exponential * jitterRatio * random);
	} else {
		const factor = 1 - jitterRatio + 2 * jitterRatio * random;
		delayMs = Math.round(exponential * factor);
		if (retryAfterMs !== undefined) delayMs = Math.max(retryAfterMs, delayMs);
	}
	return { kind: "wait", delayMs: Math.min(delayMs, MAX_TIMER_DELAY_MS) };
}

function retryWouldExceedDeadline(delayMs: number, options: ProviderRetryExecutionOptions): boolean {
	return options.deadlineAtMs !== undefined && (options.now ?? Date.now)() + delayMs > options.deadlineAtMs;
}

function retryDeadlineReached(options: ProviderRetryExecutionOptions): boolean {
	return options.deadlineAtMs !== undefined && (options.now ?? Date.now)() >= options.deadlineAtMs;
}

/**
 * One-shot completion with the shared retry policy, for consumers outside the
 * AgentSession auto-retry loop (provider SDKs never retry internally).
 */
export async function completeWithProviderRetry(
	attemptCompletion: () => Promise<AssistantMessage>,
	options: ProviderRetryExecutionOptions = {},
): Promise<AssistantMessage> {
	const policy = options.policy ?? DEFAULT_PROVIDER_RETRY_POLICY;
	const maxRetries = policy.enabled ? policy.maxRetries : 0;
	let retriesPerformed = 0;
	for (;;) {
		const message = await attemptCompletion();
		if (message.stopReason !== "error") {
			return message;
		}
		if (options.signal?.aborted) {
			// A cancel that raced the failure is an abort, not a provider failure.
			return { ...message, stopReason: "aborted" };
		}
		if (retriesPerformed >= maxRetries || isAgentLifecycleFailure(message) || isFauxProviderQueueExhausted(message)) {
			return message;
		}
		const kind = providerStreamFailureKind(message);
		if (isPermanentProviderFailureKind(kind, retriesPerformed)) {
			return message;
		}
		const delay = providerRetryDelay(
			retriesPerformed + 1,
			providerStreamFailureRetryAfterMs(message),
			policy,
			options,
		);
		if (delay.kind === "exceeds-cap" || retryWouldExceedDeadline(delay.delayMs, options)) {
			return message;
		}
		try {
			await (options.sleep ?? sleep)(delay.delayMs, options.signal);
		} catch {
			return { ...message, stopReason: "aborted" };
		}
		if (options.signal?.aborted) return { ...message, stopReason: "aborted" };
		// Injected and real timers may resume late. Do not dispatch a provider call
		// after the request's owner deadline; preserve the original provider error.
		if (retryDeadlineReached(options)) return message;
		retriesPerformed++;
	}
}

export const DEFAULT_PROVIDER_RETRY_POLICY: ProviderRetryPolicy = {
	enabled: true,
	maxRetries: 3,
	baseDelayMs: DEFAULT_PROVIDER_RETRY_BASE_DELAY_MS,
	maxRetryDelayMs: 60000,
};

/** Unary provider requests throw instead of returning an assistant error message. */
export async function requestWithProviderRetry<T>(
	attemptRequest: () => Promise<T>,
	options: ProviderRetryExecutionOptions = {},
): Promise<T> {
	const policy = options.policy ?? DEFAULT_PROVIDER_RETRY_POLICY;
	const maxRetries = policy.enabled ? policy.maxRetries : 0;
	for (let attempt = 0; ; attempt++) {
		options.signal?.throwIfAborted();
		try {
			return await attemptRequest();
		} catch (error) {
			options.signal?.throwIfAborted();
			const status = error !== null && typeof error === "object" && "status" in error ? error.status : undefined;
			const retryAfter =
				error !== null && typeof error === "object" && "retryAfterMs" in error ? error.retryAfterMs : undefined;
			const transient =
				(typeof status === "number" && (status === 408 || status === 429 || status >= 500)) ||
				(error instanceof TypeError && /fetch|network|socket/i.test(error.message));
			if (!transient || attempt >= maxRetries) throw error;
			const delay = providerRetryDelay(
				attempt + 1,
				typeof retryAfter === "number" ? retryAfter : undefined,
				policy,
				options,
			);
			if (delay.kind === "exceeds-cap" || retryWouldExceedDeadline(delay.delayMs, options)) throw error;
			await (options.sleep ?? sleep)(delay.delayMs, options.signal);
			options.signal?.throwIfAborted();
			// Keep the original request failure if the backoff overshot its owner deadline.
			if (retryDeadlineReached(options)) throw error;
		}
	}
}
