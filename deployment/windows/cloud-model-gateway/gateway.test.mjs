import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  AZURE_VISION_POLICIES,
  azureAuthenticationUnavailableError,
  containsOpaqueAzureInput,
  estimateAzureReservation,
  estimateReservation,
  inspectAzureMultimodalInput,
  forceMaxReasoning,
  normalizeModelRequest,
  FOUNDRY_KIMI_GLM_LIMITS,
  mapUpstreamStatus,
  resolveUpstreamModel,
  RollingLimiter,
  OLLAMA_ALLOWED_MODELS,
  OLLAMA_MODEL_ALIASES,
  upstreamAuthenticationUnavailablePayload,
} from "./gateway.mjs";

test("maps every upstream authentication status away from Prime authentication handling", () => {
  assert.equal(mapUpstreamStatus(401), 503);
  assert.equal(mapUpstreamStatus(403), 503);
  assert.equal(mapUpstreamStatus(400), 400);
  assert.equal(mapUpstreamStatus(429), 429);
  assert.equal(mapUpstreamStatus(500), 500);

  const helperFailure = azureAuthenticationUnavailableError();
  assert.equal(helperFailure.statusCode, 503);
  assert.match(helperFailure.message, /Azure authentication is temporarily unavailable/);

  const payload = upstreamAuthenticationUnavailablePayload();
  assert.match(payload.error.message, /protected azure-login\.ps1 helper/);
  assert.doesNotMatch(payload.error.message, /\b(?:401|403)\b/);
});

test("forces max reasoning for chat and responses payloads", () => {
  const chat = forceMaxReasoning({ reasoning_effort: "low" }, "chat");
  assert.equal(chat.reasoning_effort, "max");
  const responses = forceMaxReasoning({ reasoning: { effort: "low", summary: "detailed" } }, "responses");
  assert.deepEqual(responses.reasoning, { effort: "max", summary: "detailed" });
  assert.equal(forceMaxReasoning({}, "chat").reasoning_effort, "max");
  assert.deepEqual(forceMaxReasoning({}, "responses").reasoning, { effort: "max", summary: "auto" });
});

test("preserves selected Kimi reasoning while omitting fixed sampling overrides", () => {
  const body = {
    reasoning_effort: "low",
    temperature: 0.2,
    top_p: 0.4,
    max_tokens: 32_000,
    max_completion_tokens: 4_096,
  };
  normalizeModelRequest(body, "FW-Kimi-K3", "chat");
  assert.deepEqual(body, { reasoning_effort: "low", max_tokens: 131_072 });

  const omittedMaximum = {};
  normalizeModelRequest(omittedMaximum, "FW-Kimi-K3", "chat");
  assert.deepEqual(omittedMaximum, { reasoning_effort: "max", max_tokens: 131_072 });
});

test("enforces full GLM output while preserving sampling and tool reasoning history", () => {
  const messages = [
    { role: "user", content: "inspect" },
    {
      role: "assistant",
      content: null,
      reasoning_content: "I should inspect the file.",
      tool_calls: [{ id: "call-1", type: "function", function: { name: "read_file", arguments: "{\"path\":\"a.txt\"}" } }],
    },
    { role: "tool", tool_call_id: "call-1", content: "contents" },
  ];
  const glm = {
    reasoning_effort: "low",
    temperature: 0.2,
    top_p: 0.4,
    max_tokens: 32_000,
    max_completion_tokens: 4_096,
    max_output_tokens: 8_192,
    messages,
  };
  normalizeModelRequest(glm, "FW-GLM-5.3", "chat");
  assert.deepEqual(glm, {
    reasoning_effort: "low",
    temperature: 0.2,
    top_p: 0.4,
    max_tokens: 131_072,
    messages,
  });
});

test("pins stable Prime names through the authoritative Ollama route contract", () => {
  assert.deepEqual([...OLLAMA_ALLOWED_MODELS], ["deepseek-v4-flash", "deepseek-v4-pro", "glm-5.2", "glm-5.3", "glm-5.3-flash", "deepseek-v4.1-flash"]);
  assert.equal(resolveUpstreamModel("deepseek-v4-flash", OLLAMA_MODEL_ALIASES), "deepseek-v4-flash:0731");
  assert.equal(resolveUpstreamModel("deepseek-v4-pro", OLLAMA_MODEL_ALIASES), "deepseek-v4-pro:0813-cloud");
  assert.equal(resolveUpstreamModel("glm-5.2", OLLAMA_MODEL_ALIASES), "glm-5.2");
  assert.equal(resolveUpstreamModel("glm-5.3", OLLAMA_MODEL_ALIASES), "glm-5.3:cloud");
});

test("pins DeepSeek V4 Pro context, output, and max-only reasoning contract", () => {
  const config = JSON.parse(readFileSync(new URL("./model-contract.fixture.json", import.meta.url), "utf8"));
  const provider = config.providers["ollama-cloud"];
  const pro = provider.models.find((model) => model.id === "deepseek-v4-pro");
  assert.ok(pro, "Prime-visible deepseek-v4-pro entry is required");
  assert.equal(pro.name, "Ollama Cloud - DeepSeek V4 Pro (Max)");
  assert.equal(pro.reasoning, true);
  assert.deepEqual(pro.thinkingLevelMap, {
    off: null,
    minimal: null,
    low: null,
    medium: null,
    high: null,
    xhigh: null,
    max: "max",
  });
  assert.deepEqual(pro.input, ["text"]);
  assert.equal(pro.contextWindow, 1_000_000);
  assert.equal(pro.maxTokens, 65_536);
  assert.equal(pro.compat.requiresReasoningContentOnAssistantMessages, true);
});

test("pins GLM 5.3 context, output, text-only, and max-only reasoning contract", () => {
  const config = JSON.parse(readFileSync(new URL("./model-contract.fixture.json", import.meta.url), "utf8"));
  const provider = config.providers["ollama-cloud"];
  const glm = provider.models.find((model) => model.id === "glm-5.3");
  assert.ok(glm, "Prime-visible glm-5.3 entry is required");
  assert.equal(glm.name, "Ollama Cloud - GLM 5.3 (Max)");
  assert.equal(glm.reasoning, true);
  assert.deepEqual(glm.thinkingLevelMap, {
    off: null,
    minimal: null,
    low: null,
    medium: null,
    high: null,
    xhigh: null,
    max: "max",
  });
  assert.deepEqual(glm.input, ["text"]);
  assert.equal(glm.contextWindow, 1_000_000);
  assert.equal(glm.maxTokens, 131_072);

  const request = {
    reasoning_effort: "low",
    clear_thinking: false,
    max_tokens: 32_000,
    max_completion_tokens: 4_096,
  };
  normalizeModelRequest(request, "glm-5.3", "chat");
  assert.deepEqual(request, {
    reasoning_effort: "max",
    clear_thinking: true,
    max_tokens: 131_072,
  });
});

test("uses identical local ceilings for Foundry Kimi and GLM", () => {
  assert.deepEqual(FOUNDRY_KIMI_GLM_LIMITS, {
    tokensPerMinute: 1_899_050,
    requestsPerMinute: 1_899,
    maxInFlight: 8,
  });
});

test("pins the Kimi context, output, and reasoning replay contract", () => {
  const config = JSON.parse(readFileSync(new URL("./model-contract.fixture.json", import.meta.url), "utf8"));
  const provider = config.providers["azure-foundry-managed"];
  const kimi = provider.models.find((model) => model.id === "FW-Kimi-K3");
  assert.equal(kimi.reasoning, true);
  assert.equal(kimi.thinkingLevelMap.max, "max");
  assert.deepEqual(kimi.input, ["text", "image"]);
  assert.equal(kimi.contextWindow, 1_048_576);
  assert.equal(kimi.maxTokens, 131_072);
  assert.equal(kimi.compat.supportsReasoningEffort, true);
  assert.equal(kimi.compat.requiresReasoningContentOnAssistantMessages, true);
  assert.equal(provider.compat.maxTokensField, "max_tokens");
});

test("pins GPT-5.6 Sol as the other Azure vision model", () => {
  const config = JSON.parse(readFileSync(new URL("./model-contract.fixture.json", import.meta.url), "utf8"));
  const gpt = config.providers["azure-openai-managed"].models.find((model) => model.id === "gpt-5.6-sol");
  assert.deepEqual(gpt.input, ["text", "image"]);
  assert.equal(gpt.contextWindow, 1_050_000);
  assert.equal(gpt.maxTokens, 128_000);
  const allOtherModels = Object.entries(config.providers).flatMap(([providerId, provider]) =>
    provider.models.filter((model) => !["FW-Kimi-K3", "gpt-5.6-sol", "gpt-6-astra"].includes(model.id)).map((model) => [providerId, model]),
  );
  for (const [providerId, model] of allOtherModels) {
    assert.deepEqual(model.input, ["text"], `${providerId}/${model.id} must remain text-only`);
  }
});

test("pins GPT-6 Astra to the Sol Azure Responses contract with max reasoning", () => {
  const config = JSON.parse(readFileSync(new URL("./model-contract.fixture.json", import.meta.url), "utf8"));
  const astra = config.providers["azure-openai-managed"].models.find((model) => model.id === "gpt-6-astra");
  assert.ok(astra, "gpt-6-astra must be configured");
  assert.equal(astra.reasoning, true);
  assert.equal(astra.thinkingLevelMap.max, "max");
  assert.deepEqual(astra.input, ["text", "image"]);
  assert.equal(astra.contextWindow, 1_050_000);
  assert.equal(astra.maxTokens, 128_000);
  assert.equal(AZURE_VISION_POLICIES.get("gpt-6-astra").partType, "input_image");
});

test("pins the Foundry GLM context, output, and reasoning contract", () => {
  const config = JSON.parse(readFileSync(new URL("./model-contract.fixture.json", import.meta.url), "utf8"));
  const provider = config.providers["azure-foundry-managed"];
  const glm = provider.models.find((model) => model.id === "FW-GLM-5.3");
  assert.ok(glm, "FW-GLM-5.3 must be configured");
  assert.equal(provider.models.some((model) => model.id === "FW-GLM-5.2"), false);
  assert.equal(glm.reasoning, true);
  assert.equal(glm.thinkingLevelMap.max, "max");
  assert.deepEqual(glm.input, ["text"]);
  assert.equal(glm.contextWindow, 1_048_576);
  assert.equal(glm.maxTokens, 131_072);
  assert.equal(provider.compat.supportsReasoningEffort, true);
  assert.equal(provider.compat.maxTokensField, "max_tokens");
});

test("reservation estimates input tokens and caps only the accounting allowance", () => {
  const expected = (body, output) => Math.ceil(Math.ceil(Buffer.byteLength(JSON.stringify(body), "utf8") / 3.5) + output);

  const body = { model: "x", messages: [{ role: "user", content: "hello" }], max_tokens: 1000 };
  assert.equal(estimateReservation(body), expected(body, 1000));
  assert.equal(body.max_tokens, 1000, "reservation must not change the forwarded output limit");

  const noExplicitMaximum = { model: "x", messages: [{ role: "user", content: "hello" }] };
  assert.equal(estimateReservation(noExplicitMaximum, 128_000), expected(noExplicitMaximum, 16_384));

  const alternateFields = { model: "x", max_completion_tokens: 2000, max_output_tokens: 3000 };
  assert.equal(estimateReservation(alternateFields), expected(alternateFields, 3000));
});

test("detects opaque image and file inputs without rejecting text URLs", () => {
  assert.equal(containsOpaqueAzureInput({ messages: [{ content: [{ type: "image_url", image_url: { url: "https://example.test/image.png" } }] }] }), true);
  assert.equal(containsOpaqueAzureInput({ input: [{ type: "input_file", file_id: "file-123" }] }), true);
  assert.equal(containsOpaqueAzureInput({ messages: [{ content: "https://example.test/not-an-image" }] }), false);
});

function imageFixtureBase64(kind = "png", paddingBytes = 0) {
  const signatures = {
    png: Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    jpeg: Buffer.from([255, 216, 255, 217]),
    gif: Buffer.from("GIF89a", "ascii"),
    webp: Buffer.from("RIFF0000WEBP", "ascii"),
  };
  return Buffer.concat([signatures[kind], Buffer.alloc(paddingBytes)]).toString("base64");
}

function imageDataUrl(mimeType, payload) {
  return "data:" + mimeType + ";base64," + payload;
}

test("accepts only bounded Prime-style inline images on the three vision routes", () => {
  const png = imageFixtureBase64("png");
  const kimiBody = { model: "FW-Kimi-K3", messages: [{ role: "user", content: [
    { type: "text", text: "describe" },
    { type: "image_url", image_url: { url: `data:image/png;base64,${png}` } },
  ] }] };
  const kimi = inspectAzureMultimodalInput(kimiBody, "FW-Kimi-K3");
  assert.deepEqual(kimi, {
    hasImages: true,
    imageCount: 1,
    totalBase64Chars: png.length,
    imageTokenUpperBound: 65_536,
  });
  const kimiReservation = estimateAzureReservation(kimiBody, "FW-Kimi-K3", kimi);
  assert.ok(kimiReservation >= 65_536 + 131_072);
  assert.ok(kimiReservation < 210_000);

  const gptBody = { model: "gpt-5.6-sol", input: [{ role: "user", content: [
    { type: "input_text", text: "describe" },
    { type: "input_image", detail: "auto", image_url: `data:image/png;base64,${png}` },
  ] }] };
  const gpt = inspectAzureMultimodalInput(gptBody, "gpt-5.6-sol");
  assert.equal(gpt.imageTokenUpperBound, 65_536);
  const gptReservation = estimateAzureReservation(gptBody, "gpt-5.6-sol", gpt);
  assert.ok(gptReservation >= 65_536 + 128_000);
  const astraBody = structuredClone(gptBody);
  astraBody.model = "gpt-6-astra";
  const astra = inspectAzureMultimodalInput(astraBody, "gpt-6-astra");
  assert.equal(astra.imageTokenUpperBound, 65_536);
  const astraReservation = estimateAzureReservation(astraBody, "gpt-6-astra", astra);
  assert.ok(astraReservation >= 65_536 + 128_000);
  assert.ok(AZURE_VISION_POLICIES.get("FW-Kimi-K3").maxImages * 65_536 + 131_072 <= FOUNDRY_KIMI_GLM_LIMITS.tokensPerMinute);
});

test("fails closed for unsupported Azure multimodal forms", () => {
  const png = imageFixtureBase64("png");
  assert.throws(() => inspectAzureMultimodalInput({
    messages: [{ role: "user", content: [{ type: "image_url", image_url: { url: "https://example.test/image.png" } }] }],
  }, "FW-Kimi-K3"), /Only inline base64/);
  assert.throws(() => inspectAzureMultimodalInput({
    messages: [{ role: "user", content: [{ type: "image_url", image_url: { url: `data:image/webp;base64,${png}` } }] }],
  }, "FW-Kimi-K3"), /not supported/);
  assert.throws(() => inspectAzureMultimodalInput({
    messages: [{ role: "user", content: [{ type: "image_url", image_url: { url: "data:image/png;base64,not-base64" } }] }],
  }, "FW-Kimi-K3"), /canonical base64/);
  assert.throws(() => inspectAzureMultimodalInput({
    input: [{ type: "input_file", file_id: "file-123" }],
  }, "gpt-5.6-sol"), /file inputs remain disabled/);
  assert.throws(() => inspectAzureMultimodalInput({
    input: [{ role: "user", content: [{
      type: "input_image",
      image_url: imageDataUrl("image/png", png),
    }] }],
    max_output_tokens: 128_001,
  }, "gpt-5.6-sol"), /exceeds the configured/);
  assert.throws(() => inspectAzureMultimodalInput({
    messages: [{ role: "user", content: [{
      type: "image_url",
      image_url: { url: imageDataUrl("image/png", png) },
    }] }],
    max_tokens: 131_073,
  }, "FW-Kimi-K3"), /exceeds the configured/);
  assert.throws(() => inspectAzureMultimodalInput({
    input: [{ role: "user", content: [{
      type: "input_image",
      image_url: imageDataUrl("image/png", png),
      file_id: "file-123",
    }] }],
  }, "gpt-5.6-sol"), /file inputs remain disabled/);
  assert.throws(() => inspectAzureMultimodalInput({
    messages: [{ role: "user", content: [{
      type: "image_url",
      image_url: { url: imageDataUrl("image/png", png) },
      file_data: "forbidden",
    }] }],
  }, "FW-Kimi-K3"), /file inputs remain disabled/);
  assert.throws(() => inspectAzureMultimodalInput({
    messages: [{ role: "user", content: [{
      type: "image_url",
      image_url: { url: imageDataUrl("image/png", png) },
      file_url: "https://example.test/forbidden.png",
    }] }],
  }, "FW-Kimi-K3"), /file inputs remain disabled/);
  assert.throws(() => inspectAzureMultimodalInput({
    messages: [{ content: [{ type: "image_url", image_url: { url: `data:image/png;base64,${png}` } }] }],
  }, "FW-GLM-5.3"), /text-only/);
});

test("accepts supported signatures and the exact GPT tool-result image position", () => {
  for (const [mimeType, kind] of [
    ["image/png", "png"],
    ["image/jpeg", "jpeg"],
    ["image/gif", "gif"],
  ]) {
    const body = { messages: [{ role: "user", content: [{
      type: "image_url",
      image_url: { url: imageDataUrl(mimeType, imageFixtureBase64(kind)) },
    }] }] };
    assert.equal(inspectAzureMultimodalInput(body, "FW-Kimi-K3").imageCount, 1);
  }
  for (const [mimeType, kind] of [
    ["image/png", "png"],
    ["image/jpeg", "jpeg"],
    ["image/gif", "gif"],
    ["image/webp", "webp"],
  ]) {
    const body = { input: [{ type: "function_call_output", call_id: "call_1", output: [{
      type: "input_image",
      detail: "auto",
      image_url: imageDataUrl(mimeType, imageFixtureBase64(kind)),
    }] }] };
    assert.equal(inspectAzureMultimodalInput(body, "gpt-5.6-sol").imageCount, 1);
  }
});

test("rejects wrong positions, signatures, types, counts, and sizes", () => {
  const png = imageFixtureBase64("png");
  assert.throws(() => inspectAzureMultimodalInput({
    messages: [{ role: "assistant", content: [{
      type: "image_url",
      image_url: { url: imageDataUrl("image/png", png) },
    }] }],
  }, "FW-Kimi-K3"), /Prime user-content positions/);
  assert.throws(() => inspectAzureMultimodalInput({
    input: [{ type: "input_image", image_url: imageDataUrl("image/png", png) }],
  }, "gpt-5.6-sol"), /outside an exact Prime-supported structure/);
  assert.throws(() => inspectAzureMultimodalInput({
    messages: [{ role: "user", content: [{ image_url: { url: imageDataUrl("image/png", png) } }] }],
  }, "FW-Kimi-K3"), /outside an exact Prime-supported structure/);
  assert.throws(() => inspectAzureMultimodalInput({
    messages: [{ role: "user", content: [{
      type: "image_url",
      image_url: { url: imageDataUrl("image/jpeg", png) },
    }] }],
  }, "FW-Kimi-K3"), /do not match the declared MIME/);
  assert.throws(() => inspectAzureMultimodalInput({
    messages: [{ role: "user", content: [{
      type: "image_url",
      image_url: { url: imageDataUrl("image/svg+xml", png) },
    }] }],
  }, "FW-Kimi-K3"), /not supported/);
  assert.throws(() => inspectAzureMultimodalInput({
    messages: [{ role: "user", content: [{
      type: "image_url",
      image_url: { url: "data:image/png;base64," },
    }] }],
  }, "FW-Kimi-K3"), /canonical base64/);
  assert.throws(() => inspectAzureMultimodalInput({
    input: [{ type: "input_file", file_id: "file-123" }],
  }, "gpt-5.6-sol"), /file inputs remain disabled/);

  const eleven = Array.from({ length: 11 }, () => ({
    type: "image_url",
    image_url: { url: imageDataUrl("image/png", png) },
  }));
  assert.throws(() => inspectAzureMultimodalInput({
    messages: [{ role: "user", content: eleven }],
  }, "FW-Kimi-K3"), /at most 10 images/);

  const tooLarge = imageFixtureBase64("png", 3_375_000);
  assert.throws(() => inspectAzureMultimodalInput({
    messages: [{ role: "user", content: [{
      type: "image_url",
      image_url: { url: imageDataUrl("image/png", tooLarge) },
    }] }],
  }, "FW-Kimi-K3"), /per-image gateway limit/);

  const aggregatePart = imageFixtureBase64("png", 2_400_000);
  const aggregateImages = Array.from({ length: 3 }, () => ({
    type: "image_url",
    image_url: { url: imageDataUrl("image/png", aggregatePart) },
  }));
  assert.throws(() => inspectAzureMultimodalInput({
    messages: [{ role: "user", content: aggregateImages }],
  }, "FW-Kimi-K3"), /payload exceeds/);
});

test("replayed images remain well below the full TPM bucket", async () => {
  const png = imageFixtureBase64("png");
  const body = { model: "FW-Kimi-K3", messages: [{ role: "user", content: [{
    type: "image_url",
    image_url: { url: imageDataUrl("image/png", png) },
  }] }] };
  const reservation = estimateAzureReservation(body, "FW-Kimi-K3");
  assert.ok(reservation < 210_000);
  const limiter = new RollingLimiter({ id: "vision-replay", ...FOUNDRY_KIMI_GLM_LIMITS });
  const prior = await limiter.admit(500_000);
  prior.release();
  let waitedMs = 0;
  for (let turn = 0; turn < 3; turn += 1) {
    const admission = await limiter.admit(reservation);
    waitedMs += admission.waitedMs;
    admission.release();
  }
  const snapshot = limiter.snapshot();
  assert.equal(snapshot.usedRequests, 4);
  assert.equal(snapshot.usedTokens, 500_000 + reservation * 3);
  assert.ok(snapshot.usedTokens < snapshot.tokensPerMinute);
  assert.ok(waitedMs < 1_000);
});

test("opaque-input scanning remains bounded for deeply nested JSON", () => {
  let nested = { type: "text", text: "safe" };
  for (let depth = 0; depth < 20_000; depth += 1) nested = { child: nested };
  assert.equal(containsOpaqueAzureInput({ input: nested }), false);
});

test("opaque-input scanning remains bounded for very wide arrays and objects", () => {
  const wideArray = Array.from({ length: 150_000 }, (_, index) => ({ type: "text", text: String(index) }));
  assert.equal(containsOpaqueAzureInput({ input: wideArray }), false);
  const wideObject = {};
  for (let index = 0; index < 150_000; index += 1) wideObject["field_" + index] = index;
  assert.equal(containsOpaqueAzureInput({ input: wideObject }), false);
});

test("gateway lifecycle helper pins the candidate build exactly", () => {
  const expected = "optimus-gateway-20260917.responses-ws.1";
  const ensure = readFileSync(new URL("./ensure-gateway.ps1", import.meta.url), "utf8");
  const source = readFileSync(new URL("./gateway.mjs", import.meta.url), "utf8");
  assert.ok(ensure.includes("expectedBuildId = '" + expected + "'"));
  assert.ok(source.includes('BUILD_ID = "' + expected + '"'));
  assert.doesNotMatch(ensure, /Stop-ScheduledTask/);
  assert.match(ensure, /Refusing to restart a running gateway automatically/);
});

test("keeps text-only Azure reservation behavior unchanged", () => {
  const body = { model: "FW-Kimi-K3", messages: [{ role: "user", content: "hello" }], max_tokens: 1000 };
  assert.equal(estimateAzureReservation(body, "FW-Kimi-K3"), estimateReservation(body, 131_072));
});

test("rolling limiter enforces token ceiling and releases in-flight slots", async () => {
  const limiter = new RollingLimiter({ id: "test", tokensPerMinute: 100, requestsPerMinute: 60_000, maxInFlight: 1 });
  const first = await limiter.admit(60);
  assert.equal(limiter.snapshot().inFlight, 1);
  first.release();
  assert.equal(limiter.snapshot().inFlight, 0);
  assert.equal(limiter.snapshot().usedTokens, 60, "release must not refund the rolling reservation");
  await assert.rejects(() => limiter.admit(101), /exceeds local TPM ceiling/);
});

test("counts a retried upstream dispatch as a separate limiter admission", async () => {
  const limiter = new RollingLimiter({ id: "retry", tokensPerMinute: 100, requestsPerMinute: 60_000, maxInFlight: 1 });
  const first = await limiter.admit(25);
  first.release();
  const retry = await limiter.admit(25);
  const snapshot = limiter.snapshot();
  assert.equal(snapshot.usedRequests, 2);
  assert.equal(snapshot.usedTokens, 50);
  assert.equal(snapshot.inFlight, 1);
  retry.release();
});

test("limiter blocks aggregate TPM and RPM until aborted", async () => {
  const tokenLimiter = new RollingLimiter({ id: "aggregate", tokensPerMinute: 100, requestsPerMinute: 60_000, maxInFlight: 2 });
  (await tokenLimiter.admit(60)).release();
  const tokenAbort = new AbortController();
  const blockedByTokens = tokenLimiter.admit(50, tokenAbort.signal);
  tokenAbort.abort();
  await assert.rejects(blockedByTokens, { name: "AbortError" });

  const requestLimiter = new RollingLimiter({ id: "rpm", tokensPerMinute: 1000, requestsPerMinute: 1, maxInFlight: 2 });
  (await requestLimiter.admit(1)).release();
  const requestAbort = new AbortController();
  const blockedByRequests = requestLimiter.admit(1, requestAbort.signal);
  requestAbort.abort();
  await assert.rejects(blockedByRequests, { name: "AbortError" });
});

test("limiter expires old records and honors Azure retry delay headers", () => {
  const limiter = new RollingLimiter({ id: "expiry", tokensPerMinute: 100, requestsPerMinute: 10, maxInFlight: 1 });
  limiter.records.push({ at: Date.now() - 60_001, tokens: 90 });
  assert.equal(limiter.snapshot().usedTokens, 0);
  const before = Date.now();
  limiter.observeResponse(429, { get: (name) => name === "x-ms-retry-after-ms" ? "2500" : null });
  assert.ok(limiter.blockedUntil >= before + 2400);
});

test("limiter preserves a deadline abort reason", async () => {
  const limiter = new RollingLimiter({ id: "deadline", tokensPerMinute: 100, requestsPerMinute: 1, maxInFlight: 1 });
  limiter.blockedUntil = Date.now() + 10_000;
  const controller = new AbortController();
  const deadline = Object.assign(new Error("deadline"), { statusCode: 504 });
  const waiting = limiter.admit(1, controller.signal);
  controller.abort(deadline);
  await assert.rejects(waiting, (error) => error === deadline && error.statusCode === 504);
});

test("limiter accepts an explicit pacing interval stricter than one RPM", () => {
  const limiter = new RollingLimiter({
    id: "strict-spacing",
    tokensPerMinute: 950,
    requestsPerMinute: 1,
    maxInFlight: 1,
    minimumSpacingMs: 63_158,
  });
  assert.equal(limiter.spacingMs, 63_158);
});
