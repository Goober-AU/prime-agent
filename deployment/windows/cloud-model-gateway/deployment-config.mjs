import { readFileSync } from "node:fs";
import { join } from "node:path";

const FIELDS = new Set(["tenantId", "subscriptionId", "azureFoundryHttpUrl", "azureOpenAiHttpUrl",
  "azureOpenAiWebSocketUrl", "azureOpenAiWebSocketResource", "websocketEnabled"]);

export function validateDeployment(value) {
  if (!value || Array.isArray(value) || typeof value !== "object" || Object.keys(value).some((key) => !FIELDS.has(key))) {
    throw new Error("Invalid gateway deployment configuration");
  }
  for (const name of ["tenantId", "subscriptionId"]) {
    if (typeof value[name] !== "string" || !/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(value[name])) {
      throw new Error(`Invalid deployment ${name}`);
    }
  }
  for (const name of ["azureFoundryHttpUrl", "azureOpenAiHttpUrl", "azureOpenAiWebSocketUrl"]) {
    let url;
    try { url = new URL(value[name]); } catch { throw new Error(`Invalid deployment ${name}`); }
    const protocol = name === "azureOpenAiWebSocketUrl" ? "wss:" : "https:";
    if (url.protocol !== protocol || url.username || url.password || url.hash || url.port ||
        !/^[a-z0-9-]+\.(?:services\.ai|cognitiveservices|openai)\.azure\.com$/.test(url.hostname)) {
      throw new Error(`Unsupported deployment ${name}`);
    }
    if (name === "azureOpenAiWebSocketUrl" && (url.pathname !== "/openai/v1/responses" || url.search)) {
      throw new Error("Azure WebSocket endpoint must use /openai/v1/responses without a query");
    }
  }
  const resource = value.azureOpenAiWebSocketResource ?? "https://ai.azure.com";
  if (value.websocketEnabled !== undefined && typeof value.websocketEnabled !== "boolean") {
    throw new Error("websocketEnabled must be a boolean");
  }
  if (!["https://ai.azure.com", "https://cognitiveservices.azure.com"].includes(resource)) {
    throw new Error("Unsupported Azure WebSocket credential audience");
  }
  return Object.freeze({ ...value, azureOpenAiWebSocketResource: resource, websocketEnabled: value.websocketEnabled ?? false });
}

export function loadDeployment(root) {
  return validateDeployment(JSON.parse(readFileSync(join(root, "deployment.json"), "utf8")));
}

// Read-only extraction for a guarded cutover. The caller owns persistence and
// must recheck the pinned source hash before copying any deployment files.
export function extractLegacyDeployment(source) {
  const capture = (pattern) => {
    const matches = [...source.matchAll(pattern)];
    if (matches.length !== 1) throw new Error("Legacy gateway configuration anchor mismatch");
    return matches[0][1];
  };
  const azureOpenAiHttpUrl = capture(/upstreamUrl: "(https:\/\/[^"\r\n]+\/openai\/responses\?[^"\r\n]+)"/g);
  const ws = new URL(azureOpenAiHttpUrl);
  ws.protocol = "wss:";
  ws.pathname = "/openai/v1/responses";
  ws.search = "";
  return validateDeployment({
    tenantId: capture(/const TENANT_ID = "([^"]+)";/g),
    subscriptionId: capture(/const SUBSCRIPTION_ID = "([^"]+)";/g),
    azureFoundryHttpUrl: capture(/upstreamUrl: "(https:\/\/[^"\r\n]+\/models\/chat\/completions\?[^"\r\n]+)"/g),
    azureOpenAiHttpUrl,
    azureOpenAiWebSocketUrl: ws.href,
    azureOpenAiWebSocketResource: "https://ai.azure.com",
  });
}
