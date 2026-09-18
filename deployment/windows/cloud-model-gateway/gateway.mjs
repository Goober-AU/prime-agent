import { appendFileSync, readFileSync, renameSync, statSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { loadDeployment } from "./deployment-config.mjs";
import { installAzureResponsesWebSockets, AZURE_RESPONSES_WS_PATH } from "./azure-responses-websocket.mjs";

const ROOT = dirname(fileURLToPath(import.meta.url));
const HOST = "127.0.0.1";
const PORT = 43120;
const VERSION = 2;
export const BUILD_ID = "optimus-gateway-monitoring-repair-20260918.1";
const APP_V01_MAX_BODY_BYTES = 32_768;
const GATEWAY_STARTED_AT = Date.now();
function localCredential(runtime) {
  return runtime.localToken ?? readFileSync(join(ROOT, "local-token.txt"), "utf8").trim();
}
const LOG_PATH = join(ROOT, "gateway.log");
const PID_PATH = join(ROOT, "gateway.pid.json");
const TOKEN_HELPER = join(ROOT, "get-azure-token.ps1");
const SECRET_HELPER = join(ROOT, "secret-store.ps1");
const WINDOWS_POWERSHELL = "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";

const MAX_BODY_BYTES = 64 * 1024 * 1024;
const UPSTREAM_TIMEOUT_MS = 20 * 60_000;
// Reservation safety: cap the reserved output allowance instead of reserving
// the model's full advertised maximum for every short response. Input bytes are
// converted to a token estimate (~3.5 bytes/token for JSON). This reservation
// controls local admission only; it does not reduce the output limit forwarded
// to the model.
const RESERVATION_MAX_OUTPUT_CAP = 16_384;
// Vision requests reserve the complete normalized output allowance. The
// smaller legacy cap remains limited to pre-existing text-only admission.
const VISION_RESERVATION_MAX_OUTPUT_CAP = 131_072;
const RESERVATION_BYTES_PER_TOKEN = 3.5;

const HOP_BY_HOP_HEADERS = new Set([
  "connection",
  "content-length",
  "content-encoding",
  "keep-alive",
  "proxy-authenticate",
  "proxy-authorization",
  "te",
  "trailer",
  "transfer-encoding",
  "upgrade",
]);

function rotateLogIfNeeded() {
  try {
    if (statSync(LOG_PATH).size > 5 * 1024 * 1024) {
      renameSync(LOG_PATH, `${LOG_PATH}.1`);
    }
  } catch {}
}

function log(event, details = {}) {
  rotateLogIfNeeded();
  const record = { at: new Date().toISOString(), event, ...details };
  appendFileSync(LOG_PATH, `${JSON.stringify(record)}\n`, "utf8");
}

function safeError(error) {
  const text = error instanceof Error ? error.message : String(error);
  return text.replace(/Bearer\s+[A-Za-z0-9._~+/=-]+/gi, "Bearer [redacted]").slice(0, 500);
}

function abortError(signal) {
  if (signal?.reason instanceof Error) return signal.reason;
  return Object.assign(new Error("Request aborted"), { name: "AbortError" });
}

function sleep(ms, signal) {
  if (signal?.aborted) return Promise.reject(abortError(signal));
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      reject(abortError(signal));
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

export class RollingLimiter {
  constructor({ id, tokensPerMinute, requestsPerMinute, maxInFlight, minimumSpacingMs = 0 }) {
    this.id = id;
    this.tokensPerMinute = tokensPerMinute;
    this.requestsPerMinute = requestsPerMinute;
    this.maxInFlight = maxInFlight;
    this.spacingMs = Math.max(60_000 / requestsPerMinute, minimumSpacingMs);
    this.records = [];
    this.inFlight = 0;
    this.nextPacedAt = 0;
    this.blockedUntil = 0;
    this.pendingAdmissions = [];
    this.changeWaiters = new Set();
  }

  purge(now) {
    const cutoff = now - 60_000;
    while (this.records.length > 0 && this.records[0].at <= cutoff) this.records.shift();
  }

  snapshot(now = Date.now()) {
    this.purge(now);
    const usedTokens = this.records.reduce((sum, record) => sum + record.tokens, 0);
    return {
      id: this.id,
      tokensPerMinute: this.tokensPerMinute,
      requestsPerMinute: this.requestsPerMinute,
      usedTokens,
      usedRequests: this.records.length,
      inFlight: this.inFlight,
      maxInFlight: this.maxInFlight,
      queuedRequests: this.pendingAdmissions.length,
      blockedUntil: this.blockedUntil || undefined,
    };
  }

  observeResponse(status, headers) {
    if (status !== 429) return;
    const retryAfterMs = Number(headers.get("retry-after-ms") ?? headers.get("x-ms-retry-after-ms"));
    const retryAfter = headers.get("retry-after");
    const retryAfterSeconds = Number(retryAfter);
    const retryAfterDate = Date.parse(retryAfter || "");
    const delayMs = Number.isFinite(retryAfterMs) && retryAfterMs > 0
      ? retryAfterMs
      : Number.isFinite(retryAfterSeconds) && retryAfterSeconds > 0
        ? retryAfterSeconds * 1000
        : Number.isFinite(retryAfterDate) && retryAfterDate > Date.now()
          ? retryAfterDate - Date.now()
          : 1000;
    this.blockedUntil = Math.max(this.blockedUntil, Date.now() + delayMs);
    this.notifyChange();
  }

  notifyChange() {
    for (const notify of this.changeWaiters) notify();
  }

  waitForChange(waitMs, signal) {
    if (signal?.aborted) return Promise.reject(abortError(signal));
    return new Promise((resolve, reject) => {
      let timer;
      const cleanup = () => {
        clearTimeout(timer);
        this.changeWaiters.delete(onChange);
        signal?.removeEventListener("abort", onAbort);
      };
      const onChange = () => { cleanup(); resolve(); };
      const onAbort = () => { cleanup(); reject(abortError(signal)); };
      this.changeWaiters.add(onChange);
      signal?.addEventListener("abort", onAbort, { once: true });
      if (Number.isFinite(waitMs)) timer = setTimeout(onChange, Math.max(1, waitMs));
    });
  }

  async admit(tokens, signal) {
    if (!Number.isFinite(tokens) || tokens <= 0) throw new Error("Invalid token reservation");
    if (tokens > this.tokensPerMinute) {
      const error = new Error(`Request reservation ${tokens} exceeds local TPM ceiling ${this.tokensPerMinute}`);
      error.statusCode = 429;
      error.retryAfterSeconds = 60;
      throw error;
    }

    const queuedAt = Date.now();
    const waiter = {};
    const waitReasons = {};
    this.pendingAdmissions.push(waiter);
    try {
    for (;;) {
      if (signal?.aborted) throw abortError(signal);
      const now = Date.now();
      this.purge(now);
      const usedTokens = this.records.reduce((sum, record) => sum + record.tokens, 0);
      const tokenReady = usedTokens + tokens <= this.tokensPerMinute;
      const requestReady = this.records.length + 1 <= this.requestsPerMinute;
      const inflightReady = this.inFlight < this.maxInFlight;
      const paceReady = now >= this.nextPacedAt;
      const blockReady = now >= this.blockedUntil;
      const fifoReady = this.pendingAdmissions[0] === waiter;

      if (fifoReady && tokenReady && requestReady && inflightReady && paceReady && blockReady) {
        this.records.push({ at: now, tokens });
        this.inFlight += 1;
        this.nextPacedAt = now + this.spacingMs;
        let released = false;
        return {
          waitedMs: Math.max(0, Date.now() - queuedAt),
          waitReasons,
          release: () => {
            if (released) return;
            released = true;
            this.inFlight = Math.max(0, this.inFlight - 1);
            this.notifyChange();
          },
        };
      }

      const reasons = fifoReady ? [
        ...(!tokenReady ? ["tokens"] : []),
        ...(!requestReady ? ["requests"] : []),
        ...(!inflightReady ? ["in_flight"] : []),
        ...(!paceReady ? ["pacing"] : []),
        ...(!blockReady ? ["cooldown"] : []),
      ] : ["fifo"];
      const waits = [];
      if (!paceReady) waits.push(this.nextPacedAt - now);
      if (!blockReady) waits.push(this.blockedUntil - now);
      if ((!tokenReady || !requestReady) && this.records.length > 0) {
        waits.push(this.records[0].at + 60_000 - now + 1);
      }
      // Only the head reserves capacity; new small requests cannot repeatedly
      // overtake a large waiting request. Releases/cancellations wake waiters.
      const waitMs = fifoReady && waits.length > 0 ? Math.max(1, ...waits) : Infinity;
      const waitStarted = Date.now();
      await this.waitForChange(waitMs, signal);
      const elapsed = Math.max(0, Date.now() - waitStarted);
      // Simultaneous constraints overlap; these durations are not additive.
      for (const reason of reasons) waitReasons[reason] = (waitReasons[reason] ?? 0) + elapsed;
    }
    } finally {
      const index = this.pendingAdmissions.indexOf(waiter);
      if (index >= 0) this.pendingAdmissions.splice(index, 1);
      this.notifyChange();
    }
  }
}

export const FOUNDRY_KIMI_GLM_LIMITS = Object.freeze({
  tokensPerMinute: 1_899_050,
  requestsPerMinute: 1_899,
  maxInFlight: 8,
});

const LIMITERS = new Map([
  ["FW-Kimi-K3", new RollingLimiter({
    id: "azure-foundry/FW-Kimi-K3",
    ...FOUNDRY_KIMI_GLM_LIMITS,
  })],
  ["FW-GLM-5.3", new RollingLimiter({
    id: "azure-foundry/FW-GLM-5.3",
    ...FOUNDRY_KIMI_GLM_LIMITS,
  })],
  ["gpt-5.6-sol", new RollingLimiter({
    id: "azure-openai/gpt-5.6-sol",
    tokensPerMinute: 9_500_000,
    requestsPerMinute: 9_500,
    maxInFlight: 32,
  })],
  ["gpt-6-astra", new RollingLimiter({
    id: "azure-openai/gpt-6-astra",
    tokensPerMinute: 9_500_000,
    requestsPerMinute: 9_500,
    maxInFlight: 32,
  })],
]);

let ACTIVE_REQUESTS = 0;

// A cold start has no knowledge of admissions made by the prior process. A
// one-minute fail-closed embargo prevents a restart from creating a quota burst.
for (const limiter of LIMITERS.values()) limiter.blockedUntil = GATEWAY_STARTED_AT + 60_000;

export const OLLAMA_ALLOWED_MODELS = new Set([
  "deepseek-v4-flash",
  "deepseek-v4-pro",
  "glm-5.2",
  "glm-5.3",
  "glm-5.3-flash",
  "deepseek-v4.1-flash",
]);

export const OLLAMA_MODEL_ALIASES = new Map([
  ["deepseek-v4-flash", "deepseek-v4-flash:0731"],
  ["deepseek-v4-pro", "deepseek-v4-pro:0813-cloud"],
  ["glm-5.3", "glm-5.3:cloud"],
  ["glm-5.3-flash", "glm-5.3-flash:cloud"],
  ["deepseek-v4.1-flash", "deepseek-v4.1-flash:cloud"],
]);

export function createRoutes(deployment) { return new Map([
  ["/azure-foundry/v1/chat/completions", {
    name: "azure-foundry-chat",
    allowedModels: new Set(["FW-Kimi-K3", "FW-GLM-5.3"]),
    upstreamUrl: deployment.azureFoundryHttpUrl,
    azureResource: "https://ai.azure.com",
    forceReasoning: "chat",
  }],
  ["/azure-openai/v1/responses", {
    name: "azure-openai-responses",
    allowedModels: new Set(["gpt-5.6-sol", "gpt-6-astra"]),
    upstreamUrl: deployment.azureOpenAiHttpUrl,
    webSocketUrl: deployment.azureOpenAiWebSocketUrl,
    webSocketResource: deployment.azureOpenAiWebSocketResource,
    azureResource: "https://cognitiveservices.azure.com",
    forceReasoning: "responses",
  }],
  ["/ollama/v1/chat/completions", {
    name: "ollama-cloud",
    allowedModels: OLLAMA_ALLOWED_MODELS,
    modelAliases: OLLAMA_MODEL_ALIASES,
    upstreamUrl: "https://ollama.com/v1/chat/completions",
    ollama: true,
    forceReasoning: "chat",
  }],
]);

}

const MODEL_MAX_OUTPUT_TOKENS = new Map([
  ["FW-Kimi-K3", 131_072],
  ["FW-GLM-5.3", 131_072],
  ["gpt-5.6-sol", 128_000],
  ["gpt-6-astra", 128_000],
  ["deepseek-v4-pro", 65_536],
  ["glm-5.3", 131_072],
  ["glm-5.3-flash", 131_072],
]);

// Multimodal tokenization is model-specific and cannot be inferred safely from
// compressed/base64 bytes. Kimi K3's published processor caps one image at
// 65,536 media tokens before any tighter merge-based accounting. Use that same
// deliberately conservative upper bound for both Azure vision routes. Opaque
// base64 payload characters are removed from the normal text-byte estimate.
export const AZURE_VISION_POLICIES = new Map([
  ["FW-Kimi-K3", Object.freeze({
    partType: "image_url",
    mimeTypes: Object.freeze(["image/png", "image/jpeg", "image/gif"]),
    maxImages: 10,
    maxBase64CharsPerImage: 4_500_000,
    maxTotalBase64Chars: 9_000_000,
    imageTokenUpperBound: 65_536,
  })],
  ["gpt-5.6-sol", Object.freeze({
    partType: "input_image",
    mimeTypes: Object.freeze(["image/png", "image/jpeg", "image/webp", "image/gif"]),
    maxImages: 10,
    maxBase64CharsPerImage: 4_500_000,
    maxTotalBase64Chars: 45_000_000,
    imageTokenUpperBound: 65_536,
  })],
  ["gpt-6-astra", Object.freeze({
    partType: "input_image",
    mimeTypes: Object.freeze(["image/png", "image/jpeg", "image/webp", "image/gif"]),
    maxImages: 10,
    maxBase64CharsPerImage: 4_500_000,
    maxTotalBase64Chars: 45_000_000,
    imageTokenUpperBound: 65_536,
  })],
]);

// Prime can apply a generic 32K output fallback before a request reaches the
// gateway. Restore the verified Foundry ceilings and give DeepSeek V4 Pro its
// provider-reported maximum of 65,536 output tokens. The exact checkpoint
// rejected 131,072 and named 65,536 as its maximum.
const FORCE_FULL_OUTPUT_MODELS = new Set([
  "FW-Kimi-K3",
  "FW-GLM-5.3",
  "deepseek-v4-pro",
  "glm-5.3",
  "glm-5.3-flash",
]);

// Ollama's GLM 5.3 chat contract recommends clearing prior hidden thinking on
// each request while retaining the visible conversation and tool history.
const CLEAR_THINKING_MODELS = new Set(["glm-5.3", "glm-5.3-flash"]);

// Kimi K3 fixes its sampling values internally. Omit caller-provided sampling
// overrides while retaining the caller's supported reasoning effort.
const OMIT_FIXED_SAMPLING_MODELS = new Set(["FW-Kimi-K3"]);

const azureTokenCache = new Map();
const azureTokenRefreshes = new Map();
let ollamaKey;
let ollamaKeyPromise;


// Optional, authenticated App V0.1 model-request control. Legacy callers keep existing defaults.
export function parseAppV01OperationControl(headers, now = Date.now()) {
  const operationId = headers["x-routeworld-operation-id"];
  const rawDeadline = headers["x-routeworld-deadline-unix-ms"];
  if (operationId === undefined && rawDeadline === undefined) return undefined;
  if (typeof operationId !== "string" || !/^[A-Za-z0-9_.:-]{1,128}$/.test(operationId) ||
      typeof rawDeadline !== "string" || !/^[0-9]{1,16}$/.test(rawDeadline)) {
    throw Object.assign(new Error("Invalid App V0.1 operation headers"), { statusCode: 400 });
  }
  const deadlineUnixMs = Number(rawDeadline);
  if (!Number.isSafeInteger(deadlineUnixMs) || deadlineUnixMs > now + UPSTREAM_TIMEOUT_MS) {
    throw Object.assign(new Error("App V0.1 model deadline must remain inside the existing 20-minute request limit"), { statusCode: 400 });
  }
  if (deadlineUnixMs <= now) throw appV01DeadlineError();
  return Object.freeze({ operationId, deadlineUnixMs });
}

function appV01DeadlineError() {
  return Object.assign(new Error("The shared App V0.1 operation deadline expired"), { statusCode: 504 });
}

function checkAppV01Operation(control, now = (control?.now ?? Date.now)()) {
  if (!control) return;
  if (control.signal?.aborted) throw abortError(control.signal);
  if (now >= control.deadlineUnixMs) throw appV01DeadlineError();
}

// Only the fixed credential helper selected by gateway code can be passed here.
// The runtime seam is for offline process/clock tests, never HTTP input.
export function runOwnedCredentialHelper(scriptPath, args, control, runtime = {}) {
  const now = runtime.now ?? control.now ?? Date.now;
  const setTimer = runtime.setTimeout ?? setTimeout;
  const clearTimer = runtime.clearTimeout ?? clearTimeout;
  const spawnChild = runtime.spawn ?? spawn;
  try { checkAppV01Operation(control, now()); } catch (error) { return Promise.reject(error); }
  return new Promise((resolve, reject) => {
    const child = spawnChild(WINDOWS_POWERSHELL,
      ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", scriptPath, ...args],
      { cwd: ROOT, windowsHide: true, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env } });
    const output = [];
    let capturedBytes = 0;
    let pendingError;
    let settled = false;
    let timer;
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      clearTimer(timer);
      control.signal.removeEventListener("abort", onAbort);
      if (error) reject(error); else resolve(value);
    };
    const stopOwned = (error) => {
      if (settled || pendingError) return;
      pendingError = error;
      // Wait for this child's close/reap event before completing the request.
      // Never stop the gateway, an unrelated refresh, or an inherited PID.
      child.kill();
    };
    const onAbort = () => stopOwned(abortError(control.signal));
    child.once("error", () => finish(new Error("App V0.1 credential helper could not start")));
    child.once("close", (code) => {
      if (pendingError) return finish(pendingError);
      try { checkAppV01Operation(control, now()); } catch (error) { return finish(error); }
      if (code !== 0) return finish(new Error("App V0.1 credential helper failed"));
      finish(undefined, Buffer.concat(output).toString("utf8").trim());
    });
    const capture = (chunk, keep) => {
      capturedBytes += chunk.length;
      if (capturedBytes > 32_768) return stopOwned(new Error("App V0.1 credential helper output exceeded its bound"));
      if (keep && !pendingError) output.push(Buffer.from(chunk));
    };
    child.stdout.on("data", (chunk) => capture(chunk, true));
    child.stderr.on("data", (chunk) => capture(chunk, false));
    control.signal.addEventListener("abort", onAbort, { once: true });
    timer = setTimer(() => stopOwned(appV01DeadlineError()), Math.max(0, control.deadlineUnixMs - now()));
    if (control.signal.aborted) onAbort();
  });
}

// App requests reuse already-valid credentials, but never join/cancel an unrelated
// shared refresh promise. New helper work belongs solely to this operation.
export function createAppV01CredentialProviders(runtime = {}) {
  const now = runtime.now ?? Date.now;
  const cache = runtime.azureTokenCache ?? azureTokenCache;
  const helper = runtime.runOwnedCredentialHelper ?? runOwnedCredentialHelper;
  const getOllamaCached = runtime.getCachedOllamaKey ?? (() => ollamaKey);
  const setOllamaCached = runtime.setCachedOllamaKey ?? ((value) => { ollamaKey = value; });
  const env = runtime.env ?? process.env;
  return {
    async getAzureCredential(resource, forceRefresh, control) {
      checkAppV01Operation(control, now());
      const cached = cache.get(resource);
      if (!forceRefresh && cached && cached.expiresAt - now() > 5 * 60_000) return cached;
      let parsed;
      try {
        parsed = JSON.parse(await helper(TOKEN_HELPER, ["-Resource", resource], control));
      } catch (error) {
        checkAppV01Operation(control, now());
        throw azureAuthenticationUnavailableError();
      }
      checkAppV01Operation(control, now());
      const expiresAt = Date.parse(parsed.expiresOn);
      if (typeof parsed.accessToken !== "string" || !parsed.accessToken || !Number.isFinite(expiresAt) || expiresAt <= now()) {
        throw azureAuthenticationUnavailableError();
      }
      const credential = { token: parsed.accessToken, expiresAt };
      cache.set(resource, credential);
      return credential;
    },
    async getOllamaKey(control) {
      checkAppV01Operation(control, now());
      const cached = getOllamaCached();
      if (cached) return cached;
      const fromEnvironment = env.OLLAMA_API_KEY?.trim();
      let value = fromEnvironment;
      if (!value) {
        try { value = await helper(SECRET_HELPER, ["get", "ollama"], control); }
        catch (error) {
          checkAppV01Operation(control, now());
          throw new Error("App V0.1 Ollama credential is unavailable");
        }
      }
      checkAppV01Operation(control, now());
      if (typeof value !== "string" || !value) throw new Error("App V0.1 Ollama credential is unavailable");
      setOllamaCached(value);
      return value;
    },
  };
}

function runPowerShell(scriptPath, args = []) {
  return new Promise((resolve, reject) => {
    const child = spawn(WINDOWS_POWERSHELL, [
      "-NoProfile",
      "-ExecutionPolicy",
      "Bypass",
      "-File",
      scriptPath,
      ...args,
    ], {
      cwd: ROOT,
      windowsHide: true,
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...process.env },
    });
    const stdout = [];
    const stderr = [];
    let settled = false;
    const finish = (fn, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      fn(value);
    };
    const timer = setTimeout(() => {
      child.kill();
      finish(reject, new Error(`PowerShell helper timed out: ${scriptPath}`));
    }, 30_000);
    child.stdout.on("data", (chunk) => stdout.push(chunk));
    child.stderr.on("data", (chunk) => stderr.push(chunk));
    child.once("error", (error) => finish(reject, error));
    child.once("close", (code) => {
      const output = Buffer.concat(stdout).toString("utf8").trim();
      const errorOutput = Buffer.concat(stderr).toString("utf8").trim();
      if (code !== 0) return finish(reject, new Error(errorOutput || output || `PowerShell exited ${code}`));
      finish(resolve, output);
    });
  });
}

export function azureAuthenticationUnavailableError() {
  const error = new Error(
    "Azure authentication is temporarily unavailable. Run the protected azure-login.ps1 helper if the condition persists, then retry.",
  );
  error.statusCode = 503;
  return error;
}

async function getAzureCredential(resource, forceRefresh = false) {
  if (forceRefresh) azureTokenCache.delete(resource);
  const cached = azureTokenCache.get(resource);
  if (cached && cached.expiresAt - Date.now() > 5 * 60_000) return cached;
  if (azureTokenRefreshes.has(resource)) return azureTokenRefreshes.get(resource);
  const refresh = (async () => {
    try {
      const parsed = JSON.parse(await runPowerShell(TOKEN_HELPER, ["-Resource", resource]));
      const expiresAt = Date.parse(parsed.expiresOn);
      if (!parsed.accessToken || !Number.isFinite(expiresAt)) throw new Error("Invalid Azure token helper response");
      const credential = { token: parsed.accessToken, expiresAt };
      azureTokenCache.set(resource, credential);
      log("azure_token_refreshed", { resource, expiresOn: new Date(expiresAt).toISOString() });
      return credential;
    } catch {
      throw azureAuthenticationUnavailableError();
    }
  })();
  azureTokenRefreshes.set(resource, refresh);
  try {
    return await refresh;
  } finally {
    if (azureTokenRefreshes.get(resource) === refresh) azureTokenRefreshes.delete(resource);
  }
}

async function getOllamaKey() {
  if (ollamaKey) return ollamaKey;
  const fromEnvironment = process.env.OLLAMA_API_KEY?.trim();
  if (fromEnvironment) {
    ollamaKey = fromEnvironment;
    return ollamaKey;
  }
  if (!ollamaKeyPromise) {
    ollamaKeyPromise = runPowerShell(SECRET_HELPER, ["get", "ollama"])
      .then((value) => {
        if (!value) throw new Error("Ollama API key is not configured");
        ollamaKey = value;
        return value;
      })
      .finally(() => {
        ollamaKeyPromise = undefined;
      });
  }
  return ollamaKeyPromise;
}

function bearerToken(request) {
  const header = request.headers.authorization || "";
  return header.startsWith("Bearer ") ? header.slice("Bearer ".length) : "";
}

function sendJson(response, statusCode, payload, extraHeaders = {}) {
  const body = JSON.stringify(payload);
  response.writeHead(statusCode, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(body),
    ...extraHeaders,
  });
  response.end(body);
}

async function readBody(request, control) {
  const chunks = [];
  let length = 0;
  const onAbort = () => request.destroy(abortError(control.signal));
  if (control) {
    checkAppV01Operation(control);
    control.signal.addEventListener("abort", onAbort, { once: true });
  }
  try {
    for await (const chunk of request) {
      length += chunk.length;
      if (length > (control ? APP_V01_MAX_BODY_BYTES : MAX_BODY_BYTES)) {
        const error = new Error(control ? "App V0.1 request body exceeds its byte bound" : `Request body exceeds ${MAX_BODY_BYTES} bytes`);
        error.statusCode = 413;
        throw error;
      }
      chunks.push(chunk);
    }
    checkAppV01Operation(control);
    return Buffer.concat(chunks);
  } finally {
    control?.signal.removeEventListener("abort", onAbort);
  }
}

export function forceMaxReasoning(body, kind) {
  if (kind === "responses") {
    const existing = body.reasoning && typeof body.reasoning === "object" ? body.reasoning : {};
    body.reasoning = { ...existing, effort: "max", summary: existing.summary ?? "auto" };
  } else if (kind === "chat") {
    body.reasoning_effort = "max";
  }
  return body;
}

// Model-specific capabilities, not interchangeable labels. Existing callers
// (including App V0.1) keep max when no effort is supplied.
export const AZURE_REASONING_LEVELS = Object.freeze({
  "FW-Kimi-K3": Object.freeze(["low", "high", "max"]),
  "FW-GLM-5.3": Object.freeze(["low", "high", "max"]),
  "gpt-5.6-sol": Object.freeze(["low", "medium", "high", "xhigh", "max"]),
  "gpt-6-astra": Object.freeze(["low", "medium", "high", "xhigh", "max"]),
});

export function applyModelReasoning(body, requestedModel, kind) {
  const levels = Object.hasOwn(AZURE_REASONING_LEVELS, requestedModel)
    ? AZURE_REASONING_LEVELS[requestedModel] : undefined;
  if (!levels) return forceMaxReasoning(body, kind); // Ollama stays unchanged.
  const expectedKind = requestedModel.startsWith("FW-") ? "chat" : "responses";
  if (kind !== expectedKind) {
    throw Object.assign(new Error("Azure reasoning protocol mismatch"), { statusCode: 400 });
  }
  const existing = kind === "responses" ? body.reasoning : undefined;
  if (existing !== undefined && (!existing || typeof existing !== "object" || Array.isArray(existing))) {
    throw Object.assign(new Error("reasoning must be an object"), { statusCode: 400 });
  }
  const effort = (kind === "responses" ? existing?.effort : body.reasoning_effort) ?? "max";
  if (!levels.includes(effort)) {
    throw Object.assign(new Error(`Unsupported reasoning effort for ${requestedModel}; supported: ${levels.join(", ")}`), { statusCode: 400 });
  }
  if (kind === "responses") {
    body.reasoning = { ...existing, effort, summary: existing?.summary ?? "auto" };
  } else {
    body.reasoning_effort = effort;
  }
  return body;
}

export function normalizeModelRequest(body, requestedModel, kind) {
  if (OMIT_FIXED_SAMPLING_MODELS.has(requestedModel)) {
    delete body.temperature;
    delete body.top_p;
  }
  if (FORCE_FULL_OUTPUT_MODELS.has(requestedModel)) {
    delete body.max_completion_tokens;
    delete body.max_output_tokens;
    body.max_tokens = MODEL_MAX_OUTPUT_TOKENS.get(requestedModel);
  }
  if (CLEAR_THINKING_MODELS.has(requestedModel)) body.clear_thinking = true;
  return applyModelReasoning(body, requestedModel, kind);
}

export function resolveUpstreamModel(model, modelAliases) {
  return modelAliases?.get(model) ?? model;
}

export function estimateReservation(body, defaultMaxOutput = 16_384) {
  const serialized = JSON.stringify(body);
  const promptUpperBound = Buffer.byteLength(serialized, "utf8");
  const candidates = [body.max_output_tokens, body.max_completion_tokens, body.max_tokens]
    .map(Number)
    .filter((value) => Number.isFinite(value) && value > 0);
  const requestedMaxOutput = candidates.length > 0 ? Math.max(...candidates) : defaultMaxOutput;
  const maxOutput = Math.min(requestedMaxOutput, RESERVATION_MAX_OUTPUT_CAP);
  const promptTokenEstimate = Math.ceil(promptUpperBound / RESERVATION_BYTES_PER_TOKEN);
  return Math.ceil(promptTokenEstimate + maxOutput);
}

function collectOpaqueContent(value, output) {
  const stack = [value];
  while (stack.length > 0) {
    const current = stack.pop();
    if (Array.isArray(current)) {
      for (const item of current) stack.push(item);
      continue;
    }
    if (!current || typeof current !== "object") continue;
    const type = String(current.type || "").toLowerCase();
    const hasOpaqueKey = ["image_url", "file_id", "file_url", "file_data"].some((key) =>
      Object.prototype.hasOwnProperty.call(current, key),
  );
    if (type.includes("image") || type.includes("file") || hasOpaqueKey) output.push(current);
    for (const item of Object.values(current)) stack.push(item);
  }
}

function opaqueAzureParts(body) {
  const roots = [body?.input];
  if (Array.isArray(body?.messages)) {
    for (const message of body.messages) roots.push(message?.content);
  }
  const output = [];
  for (const root of roots) collectOpaqueContent(root, output);
  return output;
}

export function containsOpaqueAzureInput(body) {
  return opaqueAzureParts(body).length > 0;
}

function invalidAzureMultimodalInput(message) {
  const error = new Error(message);
  error.statusCode = 400;
  return error;
}

function parseCanonicalImageDataUrl(value, maxBase64Chars) {
  if (typeof value !== "string" || !value.startsWith("data:image/")) {
    throw invalidAzureMultimodalInput("Only inline base64 image data URLs emitted by Prime are allowed on Azure vision routes.");
  }
  const comma = value.indexOf(",");
  if (comma < 0) throw invalidAzureMultimodalInput("Malformed inline image data URL.");
  const metadata = value.slice(5, comma).toLowerCase();
  if (!metadata.endsWith(";base64") || metadata.indexOf(";") !== metadata.length - ";base64".length) {
    throw invalidAzureMultimodalInput("Inline Azure images must use a simple image MIME type and base64 encoding.");
  }
  const mimeType = metadata.slice(0, -";base64".length);
  const payload = value.slice(comma + 1);
  if (payload.length > maxBase64Chars) {
    throw invalidAzureMultimodalInput("One inline image exceeds the conservative per-image gateway limit.");
  }
  const bytes = Buffer.from(payload, "base64");
  const canonical = payload.length > 0 && payload.length % 4 === 0 &&
    bytes.toString("base64") === payload;
  if (!canonical) throw invalidAzureMultimodalInput("Inline Azure image payload is not canonical base64.");
  return { mimeType, base64Chars: payload.length, bytes };
}

function imageSignatureMatches(mimeType, bytes) {
  if (mimeType === "image/png") {
    return bytes.length >= 8 && bytes.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]));
  }
  if (mimeType === "image/jpeg") {
    return bytes.length >= 3 && bytes[0] === 255 && bytes[1] === 216 && bytes[2] === 255;
  }
  if (mimeType === "image/gif") {
    const signature = bytes.subarray(0, 6).toString("ascii");
    return signature === "GIF87a" || signature === "GIF89a";
  }
  if (mimeType === "image/webp") {
    return bytes.length >= 12 &&
      bytes.subarray(0, 4).toString("ascii") === "RIFF" &&
      bytes.subarray(8, 12).toString("ascii") === "WEBP";
  }
  return false;
}

function exactAzureImageParts(body, requestedModel, opaqueParts) {
  const forbiddenFileKeys = ["file_id", "file_url", "file_data"];
  if (opaqueParts.some((part) =>
    String(part.type || "").toLowerCase().includes("file") ||
    forbiddenFileKeys.some((key) => Object.prototype.hasOwnProperty.call(part, key)))) {
    throw invalidAzureMultimodalInput("Azure file inputs remain disabled; use Prime's bounded image attachment path.");
  }
  const allowed = [];
  if (requestedModel === "FW-Kimi-K3") {
    for (const message of Array.isArray(body?.messages) ? body.messages : []) {
      if (!Array.isArray(message?.content)) continue;
      for (const part of message.content) {
        if (part && typeof part === "object" && String(part.type || "").toLowerCase() === "image_url") {
          if (message.role !== "user") {
            throw invalidAzureMultimodalInput("Kimi image parts are allowed only in Prime user-content positions.");
          }
          allowed.push(part);
        }
      }
    }
  } else if (["gpt-5.6-sol", "gpt-6-astra"].includes(requestedModel)) {
    for (const item of Array.isArray(body?.input) ? body.input : []) {
      if (item?.role === "user" && Array.isArray(item.content)) {
        for (const part of item.content) {
          if (part && typeof part === "object" && String(part.type || "").toLowerCase() === "input_image") {
            allowed.push(part);
          }
        }
      } else if (item?.type === "function_call_output" && Array.isArray(item.output)) {
        for (const part of item.output) {
          if (part && typeof part === "object" && String(part.type || "").toLowerCase() === "input_image") {
            allowed.push(part);
          }
        }
      }
    }
  }
  const allowedSet = new Set(allowed);
  if (allowed.length !== opaqueParts.length || opaqueParts.some((part) => !allowedSet.has(part))) {
    throw invalidAzureMultimodalInput("Azure image or file input appeared outside an exact Prime-supported structure.");
  }
  return allowed;
}

export function inspectAzureMultimodalInput(body, requestedModel) {
  const opaqueParts = opaqueAzureParts(body);
  if (opaqueParts.length === 0) {
    return { hasImages: false, imageCount: 0, totalBase64Chars: 0, imageTokenUpperBound: undefined };
  }
  const policy = AZURE_VISION_POLICIES.get(requestedModel);
  if (!policy) {
    throw invalidAzureMultimodalInput(`Model ${String(requestedModel)} is configured as text-only on this Azure gateway.`);
  }
  const configuredMaxOutput = MODEL_MAX_OUTPUT_TOKENS.get(requestedModel);
  for (const [field, rawValue] of [
    ["max_output_tokens", body.max_output_tokens],
    ["max_completion_tokens", body.max_completion_tokens],
    ["max_tokens", body.max_tokens],
  ]) {
    const value = Number(rawValue);
    if (Number.isFinite(value) && value > configuredMaxOutput) {
      throw invalidAzureMultimodalInput(
        `Azure multimodal ${field} exceeds the configured ${requestedModel} maximum of ${configuredMaxOutput}.`,
      );
    }
  }
  const parts = exactAzureImageParts(body, requestedModel, opaqueParts);
  let imageCount = 0;
  let totalBase64Chars = 0;
  for (const part of parts) {
    const type = String(part.type || "").toLowerCase();
    if (type.includes("file")) {
      throw invalidAzureMultimodalInput("Azure file inputs remain disabled; use Prime's bounded image attachment path.");
    }
    if (type !== policy.partType) {
      throw invalidAzureMultimodalInput(`Unexpected Azure image part type ${type || "(missing)"} for ${requestedModel}.`);
    }
    const dataUrl = type === "image_url" ? part.image_url?.url : part.image_url;
    const parsed = parseCanonicalImageDataUrl(dataUrl, policy.maxBase64CharsPerImage);
    if (!policy.mimeTypes.includes(parsed.mimeType)) {
      throw invalidAzureMultimodalInput(`Image MIME type ${parsed.mimeType} is not supported for ${requestedModel}.`);
    }
    if (!imageSignatureMatches(parsed.mimeType, parsed.bytes)) {
      throw invalidAzureMultimodalInput("Inline image bytes do not match the declared MIME type.");
    }
    imageCount += 1;
    totalBase64Chars += parsed.base64Chars;
  }
  if (imageCount > policy.maxImages) {
    throw invalidAzureMultimodalInput(`${requestedModel} accepts at most ${policy.maxImages} images per request.`);
  }
  if (totalBase64Chars > policy.maxTotalBase64Chars) {
    throw invalidAzureMultimodalInput(`Inline image payload exceeds the conservative ${requestedModel} gateway limit.`);
  }
  return { hasImages: true, imageCount, totalBase64Chars, imageTokenUpperBound: policy.imageTokenUpperBound };
}

export function estimateAzureReservation(body, requestedModel, inspection = inspectAzureMultimodalInput(body, requestedModel)) {
  const defaultMaxOutput = MODEL_MAX_OUTPUT_TOKENS.get(requestedModel);
  if (!inspection.hasImages) return estimateReservation(body, defaultMaxOutput);
  const serializedBytes = Buffer.byteLength(JSON.stringify(body), "utf8");
  const textLikeBytes = Math.max(0, serializedBytes - inspection.totalBase64Chars);
  const candidates = [body.max_output_tokens, body.max_completion_tokens, body.max_tokens]
    .map(Number)
    .filter((value) => Number.isFinite(value) && value > 0);
  const requestedMaxOutput = candidates.length > 0 ? Math.max(...candidates) : defaultMaxOutput;
  const maxOutput = Math.min(requestedMaxOutput, VISION_RESERVATION_MAX_OUTPUT_CAP);
  const textTokenEstimate = Math.ceil(textLikeBytes / RESERVATION_BYTES_PER_TOKEN);
  const imageTokenEstimate = inspection.imageCount * inspection.imageTokenUpperBound;
  return Math.ceil(textTokenEstimate + imageTokenEstimate + maxOutput);
}

export function mapUpstreamStatus(status) {
  return status === 401 || status === 403 ? 503 : status;
}

export function upstreamAuthenticationUnavailablePayload() {
  return {
    error: {
      message: "Upstream authentication is temporarily unavailable. Run the protected azure-login.ps1 helper if this condition persists, then retry.",
      type: "gateway_error",
    },
  };
}

function copyResponseHeaders(upstream, response, limiter, reservation, waitedMs, waitReasons) {
  for (const [name, value] of upstream.headers.entries()) {
    if (!HOP_BY_HOP_HEADERS.has(name.toLowerCase())) response.setHeader(name, value);
  }
  response.setHeader("x-prime-gateway", `cloud-model-gateway/${VERSION}`);
  response.setHeader("x-prime-reserved-tokens", String(reservation));
  response.setHeader("x-prime-rate-wait-ms", String(waitedMs));
  if (waitReasons && Object.keys(waitReasons).length > 0) {
    response.setHeader("x-prime-rate-wait-reasons", Object.keys(waitReasons).join(","));
  }
  if (limiter) {
    const snapshot = limiter.snapshot();
    response.setHeader("x-prime-local-tpm", String(snapshot.tokensPerMinute));
    response.setHeader("x-prime-local-rpm", String(snapshot.requestsPerMinute));
  }
}

async function proxyRequest(request, response, route, runtime = {}) {
  const now = runtime.now ?? Date.now;
  const setTimer = runtime.setTimeout ?? setTimeout;
  const clearTimer = runtime.clearTimeout ?? clearTimeout;
  const requestedControl = parseAppV01OperationControl(request.headers, now());
  const ownedCredentials = requestedControl ? (runtime.appV01CredentialProviders ?? createAppV01CredentialProviders()) : undefined;
  const azureCredentialProvider = runtime.getAzureCredential ?? ownedCredentials?.getAzureCredential ?? getAzureCredential;
  const ollamaKeyProvider = runtime.getOllamaKey ?? ownedCredentials?.getOllamaKey ?? getOllamaKey;
  const upstreamFetch = runtime.fetch ?? globalThis.fetch;
  const limiterRegistry = runtime.limiters ?? LIMITERS;
  const eventLog = runtime.log ?? log;
  const controller = new AbortController();
  const control = requestedControl ? Object.freeze({ ...requestedControl, signal: controller.signal, now }) : undefined;
  response.once("close", () => {
    if (!response.writableEnded) controller.abort();
  });
  let admission;
  let totalWaitedMs = 0;
  const rateWaitReasonsMs = {};
  const recordAdmission = (value) => {
    totalWaitedMs += value.waitedMs;
    for (const [reason, ms] of Object.entries(value.waitReasons ?? {})) {
      rateWaitReasonsMs[reason] = (rateWaitReasonsMs[reason] ?? 0) + ms;
    }
  };
  let startedAt = now();
  let deadlineTimer;
  if (control) deadlineTimer = setTimer(() => controller.abort(appV01DeadlineError()),
    Math.max(0, control.deadlineUnixMs - now()));
  try {
  const rawBody = await readBody(request, control);
  let body;
  try {
    body = JSON.parse(rawBody.toString("utf8"));
  } catch {
    return sendJson(response, 400, { error: { message: "Request body must be valid JSON", type: "invalid_request_error" } });
  }

  const requestedModel = body.model;
  if (control && !new Set(["FW-Kimi-K3", "glm-5.3-flash"]).has(requestedModel)) {
    throw Object.assign(new Error("App V0.1 operation control allows only its two runtime models"), { statusCode: 400 });
  }
  if (!route.allowedModels.has(requestedModel)) {
    return sendJson(response, 400, {
      error: { message: `Model ${String(requestedModel)} is not allowed on this gateway route`, type: "invalid_request_error" },
    });
  }

  if (route.azureResource && body.n !== undefined && Number(body.n) !== 1) {
    return sendJson(response, 400, {
      error: { message: "Azure quota-safe routing supports exactly one completion per request.", type: "invalid_request_error" },
    });
  }

  let azureInputInspection;
  if (route.azureResource) {
    try {
      azureInputInspection = inspectAzureMultimodalInput(body, requestedModel);
    } catch (error) {
      return sendJson(response, 400, {
        error: { message: safeError(error), type: "invalid_request_error" },
      });
    }
  }

  const upstreamModel = resolveUpstreamModel(requestedModel, route.modelAliases);
  body.model = upstreamModel;
  normalizeModelRequest(body, requestedModel, route.forceReasoning);
  const forwardedBody = Buffer.from(JSON.stringify(body), "utf8");
  const reservation = route.azureResource
    ? estimateAzureReservation(body, requestedModel, azureInputInspection)
    : 0;
  const limiter = route.azureResource ? limiterRegistry.get(requestedModel) : undefined;
  if (!control) {
    startedAt = now();
    deadlineTimer = setTimer(() => controller.abort(Object.assign(
      new Error(`Upstream request exceeded ${UPSTREAM_TIMEOUT_MS} ms`), { statusCode: 504 })), UPSTREAM_TIMEOUT_MS);
  }
    let azureCredential;
    let ollamaCredential;
    if (route.azureResource) azureCredential = await azureCredentialProvider(route.azureResource, false, control);
    else if (route.ollama) ollamaCredential = await ollamaKeyProvider(control);

    checkAppV01Operation(control, now());
    admission = limiter ? await limiter.admit(reservation, controller.signal) : { waitedMs: 0, release() {} };
    recordAdmission(admission);
    if (azureCredential && azureCredential.expiresAt - Date.now() <= 60_000) {
      // The queue outlived the token. Preserve the first admission as a
      // conservative reservation, refresh, then obtain a fresh dispatch slot.
      admission.release();
      admission = undefined;
      azureCredential = await azureCredentialProvider(route.azureResource, true, control);
      admission = await limiter.admit(reservation, controller.signal);
      recordAdmission(admission);
    }
    const upstreamHeaders = {
      "content-type": "application/json",
      accept: request.headers.accept || "application/json",
      "user-agent": "prime-agent-cloud-model-gateway/1",
    };
    if (route.azureResource) {
      upstreamHeaders.authorization = `Bearer ${azureCredential.token}`;
      if (route.name === "azure-foundry-chat") upstreamHeaders["extra-parameters"] = "pass-through";
    } else if (route.ollama) {
      upstreamHeaders.authorization = `Bearer ${ollamaCredential}`;
    }

    checkAppV01Operation(control, now());
    let upstream = await upstreamFetch(route.upstreamUrl, {
      method: "POST",
      headers: upstreamHeaders,
      body: forwardedBody,
      signal: controller.signal,
      redirect: "manual",
    });
    limiter?.observeResponse(upstream.status, upstream.headers);
    let azureAuthenticationRetries = 0;
    if (route.azureResource && (upstream.status === 401 || upstream.status === 403)) {
      await upstream.body?.cancel();
      admission.release();
      admission = undefined;
      azureCredential = await azureCredentialProvider(route.azureResource, true, control);
      admission = await limiter.admit(reservation, controller.signal);
      recordAdmission(admission);
      upstreamHeaders.authorization = `Bearer ${azureCredential.token}`;
      azureAuthenticationRetries = 1;
      checkAppV01Operation(control, now());
      upstream = await upstreamFetch(route.upstreamUrl, {
        method: "POST",
        headers: upstreamHeaders,
        body: forwardedBody,
        signal: controller.signal,
        redirect: "manual",
      });
      limiter.observeResponse(upstream.status, upstream.headers);
      if ((upstream.status === 401 || upstream.status === 403) &&
          (!control || azureTokenCache.get(route.azureResource)?.token === azureCredential.token)) {
        azureTokenCache.delete(route.azureResource);
      }
    }
    const publicStatus = mapUpstreamStatus(upstream.status);
    copyResponseHeaders(upstream, response, limiter, reservation, totalWaitedMs, rateWaitReasonsMs);
    response.statusCode = publicStatus;
    const requestId = upstream.headers.get("x-request-id") || upstream.headers.get("apim-request-id") || undefined;
    eventLog("request", {
      route: route.name,
      model: requestedModel,
      ...(control ? { operationId: control.operationId, deadlineUnixMs: control.deadlineUnixMs } : {}),
      ...(upstreamModel !== requestedModel ? { upstreamModel } : {}),
      status: publicStatus,
      upstreamStatus: upstream.status,
      azureAuthenticationRetries,
      reservation,
      waitedMs: totalWaitedMs,
      ...(limiter ? { rateWaitReasonsMs, limiterAtResponse: limiter.snapshot() } : {}),
      durationMs: Date.now() - startedAt,
      requestId,
      remainingTokens: upstream.headers.get("x-ratelimit-remaining-tokens") || undefined,
      remainingRequests: upstream.headers.get("x-ratelimit-remaining-requests") || undefined,
    });
    if (publicStatus !== upstream.status) {
      await upstream.body?.cancel();
      response.removeHeader("www-authenticate");
      return sendJson(response, publicStatus, upstreamAuthenticationUnavailablePayload());
    }
    if (!upstream.body) return response.end();
    await pipeline(Readable.fromWeb(upstream.body), response);
  } finally {
    clearTimer(deadlineTimer);
    admission?.release();
  }
}

function healthPayload(runtime = {}) {
  const limiterRegistry = runtime.limiters ?? LIMITERS;
  return {
    status: "ok",
    version: VERSION,
    buildId: BUILD_ID,
    appV01OperationControl: 2,
    appV01ModelRequestTimeoutMs: UPSTREAM_TIMEOUT_MS,
    pid: process.pid,
    host: HOST,
    port: PORT,
    responsesWebSocket: { version: 1, path: AZURE_RESPONSES_WS_PATH,
      enabled: runtime.deployment.websocketEnabled === true,
      models: runtime.deployment.websocketEnabled === true ? ["gpt-5.6-sol", "gpt-6-astra"] : [],
      maxInFlightPerConnection: 1, store: false },
    routes: [...runtime.routes.values()].map((route) => ({ name: route.name, models: [...route.allowedModels] })),
    activeRequests: ACTIVE_REQUESTS,
    azureReasoningLevels: AZURE_REASONING_LEVELS,
    limits: [...limiterRegistry.values()].map((limiter) => limiter.snapshot()),
  };
}

export function createGatewayServer(runtime = {}) {
  const deployment = runtime.deployment ?? loadDeployment(ROOT);
  const routes = createRoutes(deployment);
  runtime = { ...runtime, routes, deployment };
  const localToken = localCredential(runtime);
  if (!localToken) throw new Error("Local gateway token is empty");
  const server = createServer(async (request, response) => {
    try {
      if (bearerToken(request) !== localToken) {
        return sendJson(response, 401, { error: { message: "Invalid local gateway credential", type: "authentication_error" } });
      }
      const url = new URL(request.url || "/", `http://${HOST}:${PORT}`);
      if (request.method === "GET" && url.pathname === "/health") return sendJson(response, 200, healthPayload(runtime));
      const route = routes.get(url.pathname.replace(/\/+$/, ""));
      if (request.method !== "POST" || !route) {
        return sendJson(response, 404, { error: { message: "Unknown gateway route", type: "invalid_request_error" } });
      }
      ACTIVE_REQUESTS += 1;
      try {
        await proxyRequest(request, response, route, runtime);
      } finally {
        ACTIVE_REQUESTS -= 1;
      }
    } catch (error) {
      const statusCode = Number(error?.statusCode) || (error?.name === "AbortError" ? 499 : 502);
      const retryAfterSeconds = Number(error?.retryAfterSeconds);
      (runtime.log ?? log)("gateway_error", { status: statusCode, error: safeError(error) });
      if (!response.headersSent) {
        sendJson(response, statusCode, {
          error: { message: safeError(error), type: statusCode === 429 ? "rate_limit_error" : "gateway_error" },
        }, Number.isFinite(retryAfterSeconds) ? { "retry-after": String(retryAfterSeconds) } : {});
      } else if (!response.writableEnded) {
        response.destroy(error instanceof Error ? error : new Error(String(error)));
      }
    }
  });
  installAzureResponsesWebSockets(server, {
    localToken, route: routes.get(AZURE_RESPONSES_WS_PATH), enabled: deployment.websocketEnabled === true,
    getAzureCredential: runtime.getAzureCredential ?? createAppV01CredentialProviders().getAzureCredential,
    limiters: runtime.limiters ?? LIMITERS, normalizeModelRequest,
    inspectAzureMultimodalInput, estimateAzureReservation,
    onActiveChange: (delta) => { ACTIVE_REQUESTS += delta; }, log: runtime.log ?? log,
    ...runtime.webSocketOptions,
  });
  return server;
}

export async function main() {
  const server = createGatewayServer();
  server.on("error", (error) => {
    log("server_error", { error: safeError(error) });
    process.stderr.write(`${safeError(error)}\n`);
    process.exitCode = 1;
  });
  server.listen(PORT, HOST, () => {
    writeFileSync(PID_PATH, JSON.stringify({ pid: process.pid, startedAt: new Date().toISOString(), version: VERSION, buildId: BUILD_ID }), "utf8");
    log("started", { pid: process.pid, host: HOST, port: PORT, version: VERSION, buildId: BUILD_ID });
  });
}

if (process.argv[1] && fileURLToPath(import.meta.url).toLowerCase() === process.argv[1].toLowerCase()) {
  await main();
}
