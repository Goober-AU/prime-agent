import { isCompactionCheckpoint, type ProviderCompactionCheckpoint } from "@earendil-works/pi-ai";

function isLegacyCheckpoint(details: object): details is Record<string, unknown> {
	return "strategy" in details && details.strategy === "openai-responses-compaction-v2";
}

export function hasProviderCheckpoint(details: unknown): boolean {
	return (
		details !== null &&
		typeof details === "object" &&
		("providerCheckpoint" in details || isLegacyCheckpoint(details))
	);
}

export function getProviderCheckpoint(details: unknown): ProviderCompactionCheckpoint | undefined {
	if (details === null || typeof details !== "object") return undefined;
	if ("providerCheckpoint" in details) {
		return isCompactionCheckpoint(details.providerCheckpoint) ? details.providerCheckpoint : undefined;
	}
	// Read older extension checkpoints without rewriting their opaque provider items.
	if (!isLegacyCheckpoint(details) || !Array.isArray(details.compactedWindow)) return undefined;
	const items: unknown[] = details.compactedWindow;
	if (
		!items.some(
			(item) =>
				item !== null &&
				typeof item === "object" &&
				"type" in item &&
				item.type === "compaction" &&
				"encrypted_content" in item &&
				typeof item.encrypted_content === "string" &&
				item.encrypted_content.length > 0,
		)
	)
		return undefined;
	let images = 0;
	const serialized = JSON.stringify(items, (key: string, value: unknown) => {
		if ((key === "image_url" || key === "url") && typeof value === "string" && value.startsWith("data:image/")) {
			images++;
			return "(image)";
		}
		return value;
	});
	const checkpoint = {
		version: 1,
		provider: details.provider,
		api: details.api,
		model: details.model,
		baseUrl: details.baseUrl,
		items,
		estimatedTokens: Math.ceil(serialized.length / 4) + images * 1200,
	};
	return isCompactionCheckpoint(checkpoint) ? checkpoint : undefined;
}
