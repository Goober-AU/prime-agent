import { streamBedrock, streamSimpleBedrock } from "./providers/amazon-bedrock.js";

import { streamBedrockResponses, streamSimpleBedrockResponses } from "./providers/amazon-bedrock-responses.js";

export const bedrockProviderModule = {
	responses: { streamBedrockResponses, streamSimpleBedrockResponses },
	streamBedrock,
	streamSimpleBedrock,
};
