import assert from "node:assert/strict";
import test from "node:test";
import { TEST_DEPLOYMENT } from "./test-deployment.mjs";
import {
  createGatewayServer,
  FOUNDRY_KIMI_GLM_LIMITS,
  RollingLimiter,
} from "./gateway.mjs";

const LOCAL_TOKEN = "isolated-vision-candidate-token";
const PNG = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl2qO0AAAAASUVORK5CYII=";

function freshLimiters() {
  return new Map([
    ["FW-Kimi-K3", new RollingLimiter({ id: "test-kimi", ...FOUNDRY_KIMI_GLM_LIMITS })],
    ["FW-GLM-5.3", new RollingLimiter({ id: "test-glm", ...FOUNDRY_KIMI_GLM_LIMITS })],
    ["gpt-5.6-sol", new RollingLimiter({
      id: "test-gpt",
      tokensPerMinute: 9_500_000,
      requestsPerMinute: 9_500,
      maxInFlight: 32,
    })],
    ["gpt-6-astra", new RollingLimiter({
      id: "test-astra",
      tokensPerMinute: 9_500_000,
      requestsPerMinute: 9_500,
      maxInFlight: 32,
    })],
  ]);
}

async function listen(server) {
  await new Promise((resolve, reject) => {
    const onError = (error) => reject(error);
    server.once("error", onError);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", onError);
      resolve();
    });
  });
  const address = server.address();
  assert.ok(address && typeof address === "object");
  return "http://127.0.0.1:" + address.port;
}

async function close(server) {
  if (!server.listening) return;
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
}

function headers() {
  return {
    authorization: "Bearer " + LOCAL_TOKEN,
    "content-type": "application/json",
  };
}

test("isolated HTTP proxy accepts, normalizes, reserves, forwards, streams, and drains a Kimi image", async (t) => {
  const captures = [];
  let markStarted;
  const started = new Promise((resolve) => { markStarted = resolve; });
  let releaseUpstream;
  const upstreamGate = new Promise((resolve) => { releaseUpstream = resolve; });
  const runtime = {
    localToken: LOCAL_TOKEN,
    limiters: freshLimiters(),
    log() {},
    async getAzureCredential() {
      return { token: "isolated-azure-token", expiresAt: Date.now() + 3_600_000 };
    },
    async fetch(url, init) {
      captures.push({
        url: String(url),
        authorization: init.headers.authorization,
        body: JSON.parse(Buffer.from(init.body).toString("utf8")),
      });
      markStarted();
      await upstreamGate;
      return new Response(JSON.stringify({ choices: [{ message: { content: "isolated-ok" } }] }), {
        status: 200,
        headers: {
          "content-type": "application/json",
          "x-request-id": "isolated-request-1",
          "x-ratelimit-remaining-tokens": "1800000",
        },
      });
    },
  };
  const server = createGatewayServer({ ...runtime, deployment: TEST_DEPLOYMENT });
  t.after(() => close(server));
  const base = await listen(server);
  const request = globalThis.fetch(base + "/azure-foundry/v1/chat/completions", {
    method: "POST",
    headers: headers(),
    body: JSON.stringify({
      model: "FW-Kimi-K3",
      messages: [{ role: "user", content: [
        { type: "text", text: "describe" },
        { type: "image_url", image_url: { url: "data:image/png;base64," + PNG } },
      ] }],
    }),
  });
  await started;

  const busyHealthResponse = await globalThis.fetch(base + "/health", { headers: headers() });
  assert.equal(busyHealthResponse.status, 200);
  const busyHealth = await busyHealthResponse.json();
  assert.equal(busyHealth.activeRequests, 1);
  assert.equal(busyHealth.limits.find((item) => item.id === "test-kimi").inFlight, 1);

  releaseUpstream();
  const response = await request;
  const responseText = await response.text();
  assert.equal(response.status, 200);
  assert.match(responseText, /isolated-ok/);
  assert.equal(captures.length, 1);
  assert.match(captures[0].url, /services\.ai\.azure\.com\/models\/chat\/completions/);
  assert.equal(captures[0].authorization, "Bearer isolated-azure-token");
  assert.equal(captures[0].body.model, "FW-Kimi-K3");
  assert.equal(captures[0].body.reasoning_effort, "max");
  assert.equal(captures[0].body.max_tokens, 131_072);
  assert.equal(captures[0].body.messages[0].content[1].image_url.url, "data:image/png;base64," + PNG);
  const reservation = Number(response.headers.get("x-prime-reserved-tokens"));
  assert.ok(reservation >= 65_536 + 131_072);
  assert.ok(reservation < 210_000);
  assert.equal(Number(response.headers.get("x-prime-local-tpm")), FOUNDRY_KIMI_GLM_LIMITS.tokensPerMinute);

  const remote = await globalThis.fetch(base + "/azure-foundry/v1/chat/completions", {
    method: "POST",
    headers: headers(),
    body: JSON.stringify({ model: "FW-Kimi-K3", messages: [{ role: "user", content: [{
      type: "image_url", image_url: { url: "https://example.test/image.png" },
    }] }] }),
  });
  assert.equal(remote.status, 400);
  assert.equal(captures.length, 1);

  const hybrid = await globalThis.fetch(base + "/azure-foundry/v1/chat/completions", {
    method: "POST",
    headers: headers(),
    body: JSON.stringify({ model: "FW-Kimi-K3", messages: [{ role: "user", content: [{
      type: "image_url",
      image_url: { url: "data:image/png;base64," + PNG },
      file_id: "forbidden",
    }] }] }),
  });
  assert.equal(hybrid.status, 400);
  assert.match((await hybrid.json()).error.message, /file inputs remain disabled/);
  assert.equal(captures.length, 1);

  const excessiveOutput = await globalThis.fetch(base + "/azure-foundry/v1/chat/completions", {
    method: "POST",
    headers: headers(),
    body: JSON.stringify({
      model: "FW-Kimi-K3",
      max_tokens: 131_073,
      messages: [{ role: "user", content: [{
        type: "image_url",
        image_url: { url: "data:image/png;base64," + PNG },
      }] }],
    }),
  });
  assert.equal(excessiveOutput.status, 400);
  assert.match((await excessiveOutput.json()).error.message, /exceeds the configured/);
  assert.equal(captures.length, 1);

  const unauthorized = await globalThis.fetch(base + "/health", {
    headers: { authorization: "Bearer wrong-token" },
  });
  assert.equal(unauthorized.status, 401);

  const idleHealth = await (await globalThis.fetch(base + "/health", { headers: headers() })).json();
  assert.equal(idleHealth.activeRequests, 0);
  assert.equal(idleHealth.limits.find((item) => item.id === "test-kimi").inFlight, 0);
});

test("isolated HTTP proxy preserves selected Foundry GLM 5.3 reasoning", async (t) => {
  const captures = [];
  const runtime = {
    localToken: LOCAL_TOKEN,
    limiters: freshLimiters(),
    log() {},
    async getAzureCredential() {
      return { token: "isolated-azure-token", expiresAt: Date.now() + 3_600_000 };
    },
    async fetch(url, init) {
      captures.push({
        url: String(url),
        authorization: init.headers.authorization,
        body: JSON.parse(Buffer.from(init.body).toString("utf8")),
      });
      return new Response(JSON.stringify({
        model: "FW-GLM-5.3",
        choices: [{ message: { content: "isolated-ok" } }],
      }), { status: 200, headers: { "content-type": "application/json" } });
    },
  };
  const server = createGatewayServer({ ...runtime, deployment: TEST_DEPLOYMENT });
  t.after(() => close(server));
  const base = await listen(server);

  const response = await globalThis.fetch(base + "/azure-foundry/v1/chat/completions", {
    method: "POST",
    headers: headers(),
    body: JSON.stringify({
      model: "FW-GLM-5.3",
      reasoning_effort: "low",
      temperature: 0.2,
      max_tokens: 32_000,
      messages: [{ role: "user", content: "reply with ok" }],
    }),
  });
  assert.equal(response.status, 200);
  assert.equal(captures.length, 1);
  assert.match(captures[0].url, /services\.ai\.azure\.com\/models\/chat\/completions\?api-version=2024-05-01-preview/);
  assert.equal(captures[0].authorization, "Bearer isolated-azure-token");
  assert.equal(captures[0].body.model, "FW-GLM-5.3");
  assert.equal(captures[0].body.reasoning_effort, "low");
  assert.equal(captures[0].body.max_tokens, 131_072);
  assert.equal(captures[0].body.temperature, 0.2);
  assert.equal("clear_thinking" in captures[0].body, false);
  assert.equal(Number(response.headers.get("x-prime-local-tpm")), FOUNDRY_KIMI_GLM_LIMITS.tokensPerMinute);

  const retired = await globalThis.fetch(base + "/azure-foundry/v1/chat/completions", {
    method: "POST",
    headers: headers(),
    body: JSON.stringify({ model: "FW-GLM-5.2", messages: [{ role: "user", content: "hello" }] }),
  });
  assert.equal(retired.status, 400);
  assert.equal(captures.length, 1);

  const image = await globalThis.fetch(base + "/azure-foundry/v1/chat/completions", {
    method: "POST",
    headers: headers(),
    body: JSON.stringify({
      model: "FW-GLM-5.3",
      messages: [{ role: "user", content: [{
        type: "image_url",
        image_url: { url: "data:image/png;base64," + PNG },
      }] }],
    }),
  });
  assert.equal(image.status, 400);
  assert.match((await image.json()).error.message, /text-only/);
  assert.equal(captures.length, 1);

  const health = await (await globalThis.fetch(base + "/health", { headers: headers() })).json();
  const foundryRoute = health.routes.find((route) => route.name === "azure-foundry-chat");
  assert.ok(foundryRoute.models.includes("FW-GLM-5.3"));
  assert.equal(foundryRoute.models.includes("FW-GLM-5.2"), false);
  assert.equal(health.limits.find((item) => item.id === "test-glm").inFlight, 0);
});

test("isolated HTTP proxy routes and reserves Sol and Astra Responses images", async (t) => {
  const captures = [];
  const runtime = {
    localToken: LOCAL_TOKEN,
    limiters: freshLimiters(),
    log() {},
    async getAzureCredential(resource) {
      assert.equal(resource, "https://cognitiveservices.azure.com");
      return { token: "isolated-gpt-token", expiresAt: Date.now() + 3_600_000 };
    },
    async fetch(url, init) {
      captures.push({
        url: String(url),
        authorization: init.headers.authorization,
        body: JSON.parse(Buffer.from(init.body).toString("utf8")),
      });
      return new Response(JSON.stringify({ output: [{ type: "message", content: [{ type: "output_text", text: "gpt-isolated-ok" }] }] }), {
        status: 200,
        headers: { "content-type": "application/json", "apim-request-id": "isolated-gpt-request" },
      });
    },
  };
  const server = createGatewayServer({ ...runtime, deployment: TEST_DEPLOYMENT });
  t.after(() => close(server));
  const base = await listen(server);
  for (const model of ["gpt-5.6-sol", "gpt-6-astra"]) {
    const response = await globalThis.fetch(base + "/azure-openai/v1/responses", {
      method: "POST",
      headers: headers(),
      body: JSON.stringify({
        model,
        input: [{ role: "user", content: [
          { type: "input_text", text: "describe" },
          { type: "input_image", detail: "auto", image_url: "data:image/png;base64," + PNG },
        ] }],
        max_output_tokens: 128_000,
      }),
    });
    assert.equal(response.status, 200);
    assert.match(await response.text(), /gpt-isolated-ok/);
    const capture = captures.at(-1);
    assert.match(capture.url, /cognitiveservices\.azure\.com\/openai\/responses/);
    assert.equal(capture.authorization, "Bearer isolated-gpt-token");
    assert.equal(capture.body.model, model);
    assert.equal(capture.body.reasoning.effort, "max");
    assert.equal(capture.body.reasoning.summary, "auto");
    assert.equal(capture.body.max_output_tokens, 128_000);
    assert.equal(capture.body.input[0].content[1].image_url, "data:image/png;base64," + PNG);
    const reservation = Number(response.headers.get("x-prime-reserved-tokens"));
    assert.ok(reservation >= 65_536 + 128_000);
    assert.ok(reservation < 210_000);
  }
  assert.equal(captures.length, 2);
  const health = await (await globalThis.fetch(base + "/health", { headers: headers() })).json();
  assert.equal(health.activeRequests, 0);
  assert.equal(health.limits.find((item) => item.id === "test-gpt").inFlight, 0);
  assert.equal(health.limits.find((item) => item.id === "test-astra").inFlight, 0);
});

test("isolated HTTP proxy pins and normalizes the GLM 5.3 cloud route", async (t) => {
  const captures = [];
  const runtime = {
    localToken: LOCAL_TOKEN,
    limiters: freshLimiters(),
    log() {},
    async getOllamaKey() {
      return "isolated-ollama-token";
    },
    async fetch(url, init) {
      captures.push({
        url: String(url),
        authorization: init.headers.authorization,
        body: JSON.parse(Buffer.from(init.body).toString("utf8")),
      });
      return new Response(JSON.stringify({
        model: "glm-5.3:cloud",
        choices: [{ finish_reason: "stop", message: { role: "assistant", content: "ok", reasoning: "bounded" } }],
      }), { status: 200, headers: { "content-type": "application/json" } });
    },
  };
  const server = createGatewayServer({ ...runtime, deployment: TEST_DEPLOYMENT });
  t.after(() => close(server));
  const base = await listen(server);
  const response = await globalThis.fetch(base + "/ollama/v1/chat/completions", {
    method: "POST",
    headers: headers(),
    body: JSON.stringify({
      model: "glm-5.3",
      messages: [{ role: "user", content: "hello" }],
      reasoning_effort: "low",
      clear_thinking: false,
      max_tokens: 32_000,
      max_completion_tokens: 4_096,
    }),
  });
  assert.equal(response.status, 200);
  const responseBody = await response.json();
  assert.equal(responseBody.model, "glm-5.3:cloud");
  assert.equal(captures.length, 1);
  assert.equal(captures[0].url, "https://ollama.com/v1/chat/completions");
  assert.equal(captures[0].authorization, "Bearer isolated-ollama-token");
  assert.equal(captures[0].body.model, "glm-5.3:cloud");
  assert.equal(captures[0].body.reasoning_effort, "max");
  assert.equal(captures[0].body.clear_thinking, true);
  assert.equal(captures[0].body.max_tokens, 131_072);
  assert.equal("max_completion_tokens" in captures[0].body, false);
  const health = await (await globalThis.fetch(base + "/health", { headers: headers() })).json();
  assert.equal(health.activeRequests, 0);
  assert.ok(health.routes.find((route) => route.name === "ollama-cloud").models.includes("glm-5.3"));
});

test("isolated HTTP proxy maps repeated Azure authentication failures without leaking credentials", async (t) => {
  let upstreamCalls = 0;
  let credentialCalls = 0;
  const runtime = {
    localToken: LOCAL_TOKEN,
    limiters: freshLimiters(),
    log() {},
    async getAzureCredential(_resource, forceRefresh) {
      credentialCalls += 1;
      return { token: forceRefresh ? "isolated-refresh-token" : "isolated-initial-token", expiresAt: Date.now() + 3_600_000 };
    },
    async fetch() {
      upstreamCalls += 1;
      return new Response("upstream unauthorized", { status: 401, headers: { "www-authenticate": "Bearer secret" } });
    },
  };
  const server = createGatewayServer({ ...runtime, deployment: TEST_DEPLOYMENT });
  t.after(() => close(server));
  const base = await listen(server);
  const response = await globalThis.fetch(base + "/azure-foundry/v1/chat/completions", {
    method: "POST",
    headers: headers(),
    body: JSON.stringify({ model: "FW-Kimi-K3", messages: [{ role: "user", content: "hello" }] }),
  });
  const body = await response.json();
  assert.equal(response.status, 503);
  assert.equal(response.headers.get("www-authenticate"), null);
  assert.equal(upstreamCalls, 2);
  assert.equal(credentialCalls, 2);
  assert.equal(body.error.type, "gateway_error");
  assert.doesNotMatch(JSON.stringify(body), /isolated-(initial|refresh)-token/);
  const health = await (await globalThis.fetch(base + "/health", { headers: headers() })).json();
  assert.equal(health.activeRequests, 0);
  assert.ok(health.limits.every((item) => item.inFlight === 0));
});
