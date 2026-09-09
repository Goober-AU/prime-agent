import { fauxAssistantMessage, getApiProvider, registerFauxProvider } from "@earendil-works/pi-ai";
import type { ExtensionAPI } from "../../src/core/extensions/types.js";

export default function telegramFauxProvider(pi: ExtensionAPI): void {
	const faux = registerFauxProvider({ provider: "telegram-faux", models: [{ id: "test", reasoning: false }] });
	faux.setResponses([
		fauxAssistantMessage("Telegram end-to-end response"),
		fauxAssistantMessage("Second Telegram response"),
	]);
	const provider = getApiProvider(faux.api);
	if (!provider) throw new Error("Faux provider was not registered");
	pi.registerProvider("telegram-faux", {
		api: faux.api,
		apiKey: "fake-key",
		baseUrl: faux.getModel().baseUrl,
		streamSimple: provider.streamSimple,
		models: faux.models,
	});
}
