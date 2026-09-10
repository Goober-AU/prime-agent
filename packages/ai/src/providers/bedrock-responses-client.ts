import { defaultProvider } from "@aws-sdk/credential-provider-node";
import { Hash } from "@smithy/core/serde";
import { SignatureV4 } from "@smithy/signature-v4";
import OpenAI from "openai";
import type { Model, StreamOptions } from "../types.js";

export interface BedrockResponsesAuthOptions extends StreamOptions {
	/** Explicit signing region; otherwise taken from the selected endpoint. */
	region?: string;
	profile?: string;
	credentialProvider?: ReturnType<typeof defaultProvider>;
}

/** Both APIs speak Responses, but Runtime and Mantle use different SigV4 service names. */
export function createBedrockResponsesClient(
	model: Model<"bedrock-responses">,
	options?: BedrockResponsesAuthOptions,
): OpenAI {
	const baseURL = process.env.AWS_BEDROCK_BASE_URL?.trim() || model.baseUrl;
	const url = new URL(baseURL);
	if (url.search || url.hash || url.username || url.password || !["http:", "https:"].includes(url.protocol)) {
		throw new Error("Bedrock base URL must be an HTTP(S) API root without credentials, query, or fragment.");
	}
	const endpoint = /^(bedrock-mantle|bedrock-runtime)\.([a-z0-9-]+)\.(?:api\.aws|amazonaws\.com)$/.exec(url.hostname);
	const modelEndpoint = /^(bedrock-mantle|bedrock-runtime)\.([a-z0-9-]+)\.(?:api\.aws|amazonaws\.com)$/.exec(
		new URL(model.baseUrl).hostname,
	);
	const runtimeModel = /^(?:global|us)\.openai\./.test(model.id);
	const service = runtimeModel ? "bedrock" : "bedrock-mantle";
	const region =
		options?.region ||
		endpoint?.[2] ||
		modelEndpoint?.[2] ||
		process.env.AWS_REGION ||
		process.env.AWS_DEFAULT_REGION;
	if (!region) throw new Error("Set AWS_REGION or pass a signing region for the Bedrock proxy.");
	if (endpoint && options?.region && options.region !== endpoint[2]) {
		throw new Error(`Bedrock endpoint region ${endpoint[2]} does not match signing region ${options.region}.`);
	}
	if (endpoint && (endpoint[1] === "bedrock-runtime") !== runtimeModel) {
		throw new Error("Use openai.gpt-6-astra with Mantle, or a global./us. inference profile with Bedrock Runtime.");
	}
	if (model.id === "openai.gpt-6-astra" && endpoint && endpoint[2] !== "us-west-2") {
		throw new Error("GPT-6 Astra on Bedrock Mantle requires us-west-2 (Oregon).");
	}
	const explicitKey = options?.apiKey === "<authenticated>" ? undefined : options?.apiKey;
	if (explicitKey && (options?.profile || options?.credentialProvider)) {
		throw new Error("Choose either a Bedrock bearer token or explicit AWS credentials.");
	}
	const bearerToken =
		explicitKey ||
		(!options?.profile && !options?.credentialProvider ? process.env.AWS_BEARER_TOKEN_BEDROCK : undefined);
	const signer = bearerToken
		? undefined
		: new SignatureV4({
				credentials:
					options?.credentialProvider || defaultProvider(options?.profile ? { profile: options.profile } : {}),
				region,
				service,
				sha256: Hash.bind(null, "sha256"),
			});
	const defaultHeaders = new Headers({ ...model.headers, ...options?.headers });
	if (defaultHeaders.has("authorization"))
		throw new Error("Use Bedrock apiKey or AWS credentials instead of an Authorization header.");
	return new OpenAI({
		apiKey: bearerToken || "<aws-sigv4>",
		organization: null,
		project: null,
		baseURL,
		defaultHeaders: Object.fromEntries(defaultHeaders),
		maxRetries: 0,
		fetch: async (input, init) => {
			const request = new Request(input, init);
			const target = new URL(request.url);
			if (target.origin !== url.origin)
				throw new Error("Refusing to send AWS credentials outside the configured Bedrock endpoint.");
			const headers = new Headers(request.headers);
			headers.delete("authorization");
			if (bearerToken) {
				headers.set("authorization", `Bearer ${bearerToken}`);
			} else if (signer) {
				const body = await request.clone().text();
				const query: Record<string, string[]> = {};
				for (const [key, value] of target.searchParams) {
					query[key] ??= [];
					query[key].push(value);
				}
				headers.set("host", target.host);
				for (const name of ["x-amz-date", "x-amz-security-token", "x-amz-content-sha256"]) headers.delete(name);
				const signed = await signer.sign({
					protocol: target.protocol,
					hostname: target.hostname,
					port: target.port ? Number(target.port) : undefined,
					method: request.method,
					path: target.pathname,
					query,
					headers: Object.fromEntries(headers),
					body,
				});
				for (const [key, value] of Object.entries(signed.headers)) headers.set(key, value);
			}
			request.signal.throwIfAborted();
			return globalThis.fetch(new Request(request, { headers, redirect: "manual" }));
		},
	});
}
