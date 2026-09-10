import type { CompactionOptions, ProviderCompactionResult } from "../compaction.js";
import { calculateCost } from "../models.js";
import type { Api, Model, Usage } from "../types.js";
import { headersToRecord } from "../utils/headers.js";
import { parseRetryAfterMs } from "../utils/stream-failure.js";

export class CompactionRequestError extends Error {
	constructor(
		message: string,
		readonly status: number,
		readonly retryAfterMs?: number,
	) {
		super(message);
		this.name = "CompactionRequestError";
	}
}

const AZURE_MANAGED_PROVIDER = "azure-openai-managed";
const AZURE_ASTRA_MODEL = "gpt-6-astra";
const AZURE_GATEWAY_BASE_PATH = "/azure-openai/v1";
const RESPONSES_COMPACT_PROTOCOL = "openai-responses-compact-v1";

function normalizeEndpoint(value: string): string | undefined {
	try {
		const url = new URL(value);
		if (!(["http:", "https:"] as string[]).includes(url.protocol)) return undefined;
		if (url.username || url.password || url.search || url.hash) return undefined;
		url.pathname = url.pathname.replace(/\/+$/, "");
		return url.toString().replace(/\/$/, "");
	} catch {
		return undefined;
	}
}

/** Return the exact endpoint only for the staged, live-validated Azure Astra route. */
export function validatedNativeCompactionEndpoint(model: Model<Api>): string | undefined {
	if (
		model.provider !== AZURE_MANAGED_PROVIDER ||
		model.id !== AZURE_ASTRA_MODEL ||
		model.api !== "openai-responses"
	) {
		return undefined;
	}
	const capability = model.nativeCompaction;
	if (
		!capability?.enabled ||
		capability.validation !== "live-verified" ||
		capability.protocol !== RESPONSES_COMPACT_PROTOCOL ||
		capability.apiVersion !== "v1" ||
		capability.provider !== model.provider ||
		capability.model !== model.id
	) {
		return undefined;
	}
	const baseUrl = normalizeEndpoint(model.baseUrl);
	const endpoint = normalizeEndpoint(capability.endpoint);
	if (!baseUrl || !endpoint) return undefined;
	const parsedBase = new URL(baseUrl);
	if (parsedBase.pathname !== AZURE_GATEWAY_BASE_PATH) return undefined;
	const expected = normalizeEndpoint(`${baseUrl}/responses/compact`);
	return endpoint === expected ? endpoint : undefined;
}

export function supportsOpenAICompaction(model: Model<Api>): boolean {
	return (
		(model.provider === "openai-codex" && model.api === "openai-codex-responses") ||
		(model.id === "gpt-6-astra" && model.provider === "openai" && model.api === "openai-responses") ||
		validatedNativeCompactionEndpoint(model) !== undefined
	);
}

function record(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === "object" && !Array.isArray(value);
}

function tokenCount(value: unknown): number {
	return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : 0;
}

function estimatedWindowChars(items: Record<string, unknown>[]): number {
	let images = 0;
	const serialized = JSON.stringify(items, (key, value: unknown) => {
		if ((key === "image_url" || key === "url") && typeof value === "string" && value.startsWith("data:image/")) {
			images++;
			return "(image)";
		}
		return value;
	});
	// Match Prime's image heuristic; base64 byte length is not model token usage.
	return serialized.length + images * 4800;
}

/** Shared unary transport. Unsupported endpoints fall back; invalid checkpoints never commit. */
export async function requestOpenAICompaction(
	model: Model<Api>,
	url: string,
	headers: Headers,
	body: Record<string, unknown>,
	options?: CompactionOptions,
	decode: (response: Response) => Promise<unknown> = (response) => response.json(),
): Promise<ProviderCompactionResult | undefined> {
	const timeout = AbortSignal.timeout(options?.timeoutMs ?? 1_200_000);
	const signal = options?.signal ? AbortSignal.any([options.signal, timeout]) : timeout;
	const replacement = await options?.onPayload?.(body, model);
	const response = await fetch(url, {
		method: "POST",
		headers,
		body: JSON.stringify(replacement ?? body),
		signal,
	});
	await options?.onResponse?.({ status: response.status, headers: headersToRecord(response.headers) }, model);
	if ([404, 405, 501].includes(response.status)) {
		await response.body?.cancel();
		return undefined;
	}
	if (!response.ok) {
		// Do not include response bodies: providers may echo prompt or credential data.
		if (response.status === 400 || response.status === 413) {
			const failure: unknown = await response.json().catch(() => undefined);
			signal.throwIfAborted();
			// Preserve the direct OpenAI/Codex fallback contract. A validated gateway
			// route may fall back only for an explicitly unsupported endpoint above;
			// size and validation failures remain visible failures.
			if (
				validatedNativeCompactionEndpoint(model) === undefined &&
				(response.status === 413 ||
					(record(failure) && record(failure.error) && failure.error.code === "context_length_exceeded"))
			) {
				return undefined;
			}
		} else {
			await response.body?.cancel();
		}
		throw new CompactionRequestError(
			`Server compaction failed (HTTP ${response.status})`,
			response.status,
			parseRetryAfterMs(response.headers),
		);
	}
	const payload: unknown = await decode(response);
	signal.throwIfAborted();
	if (payload === undefined && validatedNativeCompactionEndpoint(model) === undefined) return undefined;
	if (
		!record(payload) ||
		!Array.isArray(payload.output) ||
		!payload.output.every(record) ||
		!payload.output.some(
			(item) =>
				item.type === "compaction" &&
				typeof item.encrypted_content === "string" &&
				item.encrypted_content.length > 0,
		)
	) {
		throw new Error("Server compaction returned no valid encrypted checkpoint");
	}
	const rawUsage = record(payload.usage) ? payload.usage : undefined;
	let usage: Usage | undefined;
	if (rawUsage) {
		const inputDetails = record(rawUsage.input_tokens_details) ? rawUsage.input_tokens_details : undefined;
		const input = tokenCount(rawUsage.input_tokens);
		const cached = Math.min(input, tokenCount(inputDetails?.cached_tokens));
		usage = {
			input: input - cached,
			output: tokenCount(rawUsage.output_tokens),
			cacheRead: cached,
			cacheWrite: 0,
			totalTokens: tokenCount(rawUsage.total_tokens) || input + tokenCount(rawUsage.output_tokens),
			cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
		};
		calculateCost(model, usage);
	}
	return {
		checkpoint: {
			version: 1,
			provider: model.provider,
			api: model.api,
			model: model.id,
			baseUrl: model.baseUrl,
			...(validatedNativeCompactionEndpoint(model) ? { endpoint: url } : {}),
			items: payload.output,
			estimatedTokens: Math.max(usage?.output ?? 0, Math.ceil(estimatedWindowChars(payload.output) / 4)),
		},
		usage,
	};
}

/** Codex v2 returns one checkpoint; the client retains up to 64k tokens of recent user context. */
export function buildCodexCompactedWindow(
	input: Record<string, unknown>[],
	checkpoint: Record<string, unknown>,
): Record<string, unknown>[] {
	let remainingChars = 64_000 * 4;
	const retained: Record<string, unknown>[] = [];
	for (let i = input.length - 1; i >= 0 && remainingChars > 0; i--) {
		const item = input[i];
		if (
			(item.type !== undefined && item.type !== "message") ||
			!["user", "developer", "system"].includes(String(item.role))
		)
			continue;
		const size = estimatedWindowChars([item]);
		if (size <= remainingChars) {
			retained.push(item);
			remainingChars -= size;
			continue;
		}
		// Only the boundary message is shortened. Preserve complete image blocks when
		// they fit; never slice image data or serialized provider items.
		const content: Record<string, unknown>[] = [];
		const blocks = typeof item.content === "string" ? [{ type: "input_text", text: item.content }] : item.content;
		if (Array.isArray(blocks)) {
			for (let j = blocks.length - 1; j >= 0 && remainingChars > 128; j--) {
				const block: unknown = blocks[j];
				if (!record(block)) continue;
				const blockSize = estimatedWindowChars([block]);
				if (blockSize <= remainingChars - 128) {
					content.unshift(block);
					remainingChars -= blockSize;
				} else if (block.type === "input_text" && typeof block.text === "string") {
					content.unshift({ ...block, text: block.text.slice(-(remainingChars - 128)) });
					remainingChars = 0;
				}
			}
		}
		if (content.length > 0) retained.push({ ...item, content });
		remainingChars = 0;
	}
	return [...retained.reverse(), checkpoint];
}
