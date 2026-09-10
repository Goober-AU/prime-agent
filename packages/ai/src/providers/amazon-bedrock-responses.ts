import type { ResponseCreateParamsStreaming } from "openai/resources/responses/responses.js";
import { clampThinkingLevel } from "../models.js";
import type { Api, AssistantMessage, Context, Model, SimpleStreamOptions, StreamFunction, Usage } from "../types.js";
import { AssistantMessageEventStream } from "../utils/event-stream.js";
import { headersToRecord } from "../utils/headers.js";
import {
	formatStreamFailureMessage,
	recordStreamFailure,
	streamFailureFromStopReason,
} from "../utils/stream-failure.js";
import { type BedrockResponsesAuthOptions, createBedrockResponsesClient } from "./bedrock-responses-client.js";
import { convertResponsesMessages, convertResponsesTools, processResponsesStream } from "./openai-responses-shared.js";
import { buildBaseOptions } from "./simple-options.js";

const BEDROCK_TOOL_CALL_PROVIDERS = new Set(["amazon-bedrock"]);

export interface BedrockResponsesOptions extends BedrockResponsesAuthOptions {
	reasoningEffort?: "low" | "medium" | "high" | "xhigh" | "max";
	reasoningSummary?: "auto" | "detailed" | "concise";
}

export const streamBedrockResponses: StreamFunction<"bedrock-responses", BedrockResponsesOptions> = (
	model: Model<"bedrock-responses">,
	context: Context,
	options?: BedrockResponsesOptions,
): AssistantMessageEventStream => {
	const stream = new AssistantMessageEventStream();

	(async () => {
		const output: AssistantMessage = {
			role: "assistant",
			content: [],
			api: "bedrock-responses" as Api,
			provider: model.provider,
			model: model.id,
			usage: {
				input: 0,
				output: 0,
				cacheRead: 0,
				cacheWrite: 0,
				totalTokens: 0,
				cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
			},
			stopReason: "stop",
			timestamp: Date.now(),
		};

		try {
			const client = createBedrockResponsesClient(model, options);
			let params = buildParams(model, context, options);
			const nextParams = await options?.onPayload?.(params, model);
			if (nextParams !== undefined) {
				params = nextParams as ResponseCreateParamsStreaming;
			}
			const requestOptions = {
				...(options?.signal ? { signal: options.signal } : {}),
				...(options?.timeoutMs !== undefined ? { timeout: options.timeoutMs } : {}),
			};
			const { data: openaiStream, response } = await client.responses.create(params, requestOptions).withResponse();
			await options?.onResponse?.({ status: response.status, headers: headersToRecord(response.headers) }, model);
			const requestId = response.headers.get("x-request-id") ?? undefined;
			stream.push({ type: "start", partial: output });

			await processResponsesStream(openaiStream, output, stream, model, {
				onUsageObservation: options?.onUsageObservation,
				applyServiceTierPricing: applyBedrockAstraContextPricing,
			});

			if (options?.signal?.aborted) {
				throw new Error("Request was aborted");
			}

			if (output.stopReason === "aborted" || output.stopReason === "error") {
				throw streamFailureFromStopReason(output.stopReasonRaw, { requestId });
			}

			stream.push({ type: "done", reason: output.stopReason, message: output });
			stream.end();
		} catch (error) {
			for (const block of output.content) {
				delete (block as { index?: number }).index;
				// partialJson is only a streaming scratch buffer; never persist it.
				delete (block as { partialJson?: string }).partialJson;
			}
			output.stopReason = options?.signal?.aborted ? "aborted" : "error";
			output.errorMessage = formatStreamFailureMessage(error);
			recordStreamFailure(model, output, error);
			stream.push({ type: "error", reason: output.stopReason, error: output });
			stream.end();
		}
	})();

	return stream;
};

export const streamSimpleBedrockResponses: StreamFunction<"bedrock-responses", SimpleStreamOptions> = (
	model: Model<"bedrock-responses">,
	context: Context,
	options?: SimpleStreamOptions,
): AssistantMessageEventStream => {
	const base = buildBaseOptions(model, options);
	const reasoningEffort = clampThinkingLevel(
		model,
		options?.reasoning ?? "medium",
	) as BedrockResponsesOptions["reasoningEffort"];
	return streamBedrockResponses(model, context, {
		...base,
		onUsageObservation: options?.onUsageObservation,
		reasoningEffort,
	});
};

function buildParams(model: Model<"bedrock-responses">, context: Context, options?: BedrockResponsesOptions) {
	if (options?.serviceTier && options.serviceTier !== "default" && options.serviceTier !== "auto") {
		throw new Error("Bedrock Astra supports only the Standard service tier.");
	}
	const params: ResponseCreateParamsStreaming = {
		model: model.id,
		input: convertResponsesMessages(model, context, BEDROCK_TOOL_CALL_PROVIDERS),
		stream: true,
		store: false,
		service_tier: "default",
		max_output_tokens: options?.maxTokens,
	};
	if (context.tools?.length) params.tools = convertResponsesTools(context.tools);
	if (model.reasoning) {
		const effort = options?.reasoningEffort ?? "medium";
		if (!["low", "medium", "high", "xhigh", "max"].includes(effort)) {
			throw new Error("Bedrock Astra reasoning effort must be low, medium, high, xhigh, or max.");
		}
		params.reasoning = { effort, summary: options?.reasoningSummary ?? "auto" };
		params.include = ["reasoning.encrypted_content"];
	}
	return params;
}

function applyBedrockAstraContextPricing(usage: Usage): void {
	if (usage.input + usage.cacheRead + usage.cacheWrite <= 272_000) return;
	usage.cost.input *= 2;
	usage.cost.cacheRead *= 2;
	usage.cost.cacheWrite *= 2;
	usage.cost.output *= 1.5;
	usage.cost.total = usage.cost.input + usage.cost.output + usage.cost.cacheRead + usage.cost.cacheWrite;
}
