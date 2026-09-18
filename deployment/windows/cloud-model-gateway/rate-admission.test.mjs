import assert from "node:assert/strict";
import test from "node:test";
import { createGatewayServer, RollingLimiter } from "./gateway.mjs";
import { TEST_DEPLOYMENT } from "./test-deployment.mjs";

async function advance(t, ms = 0) {
  t.mock.timers.tick(ms);
  for (let index = 0; index < 12; index++) await Promise.resolve();
}

function fixture(t, overrides = {}) {
  t.mock.timers.enable({ apis: ["Date", "setTimeout"], now: 100_000 });
  return new RollingLimiter({ id: "isolated-admission", tokensPerMinute: 100,
    requestsPerMinute: 60_000, maxInFlight: 1, ...overrides });
}

test("new small requests cannot repeatedly overtake an older budget-blocked request", async (t) => {
  const limiter = fixture(t);
  const occupied = await limiter.admit(60);
  const order = [];
  const older = limiter.admit(50).then((admission) => { order.push("older"); return admission; });
  await advance(t, 10);
  occupied.release();
  const newer = limiter.admit(40).then((admission) => { order.push("newer"); return admission; });
  await advance(t);
  assert.deepEqual(order, [], "fresh small requests must not steal the older request's next slot");
  await advance(t, 59_991);
  const first = await older;
  assert.deepEqual(order, ["older"]);
  first.release();
  await advance(t, 1);
  const second = await newer;
  assert.deepEqual(order, ["older", "newer"]);
  assert.equal(limiter.snapshot().usedTokens, 90);
  assert.equal(limiter.snapshot().usedRequests, 2);
  second.release();
});

test("an in-flight release wakes the oldest waiter without the old 25 ms polling delay", async (t) => {
  const limiter = fixture(t);
  const first = await limiter.admit(1);
  await advance(t, 2);
  let delivered = false;
  const next = limiter.admit(1).then((admission) => { delivered = true; return admission; });
  await advance(t, 1);
  first.release();
  await advance(t);
  assert.equal(delivered, true);
  const admission = await next;
  assert.equal(admission.waitedMs, 1, "measure elapsed wait, not planned sleep intervals");
  assert.deepEqual(admission.waitReasons, { in_flight: 1 });
  admission.release();
});

test("cancelling a blocked queue head frees its successor without refunding prior reservations", async (t) => {
  const limiter = fixture(t);
  (await limiter.admit(60)).release();
  await advance(t, 2);
  const controller = new AbortController();
  const reason = Object.assign(new Error("isolated deadline"), { statusCode: 504 });
  const head = limiter.admit(50, controller.signal);
  const rejected = assert.rejects(head, (error) => error === reason);
  let delivered = false;
  const next = limiter.admit(30).then((admission) => { delivered = true; return admission; });
  await advance(t, 3);
  assert.equal(delivered, false);
  controller.abort(reason);
  await rejected;
  await advance(t);
  const admission = await next;
  assert.equal(admission.waitedMs, 3);
  assert.deepEqual(admission.waitReasons, { fifo: 3 });
  assert.equal(limiter.snapshot().usedTokens, 90);
  admission.release();
});

test("token and request windows, pacing, and provider cooldown remain mandatory", async (t) => {
  const limiter = fixture(t, { requestsPerMinute: 2, maxInFlight: 2 });
  (await limiter.admit(60)).release();
  const controller = new AbortController();
  const pending = limiter.admit(50, controller.signal);
  await advance(t, 30_000);
  assert.equal(limiter.snapshot().usedRequests, 1);
  limiter.observeResponse(429, new Headers({ "retry-after-ms": "40000" }));
  await advance(t);
  await advance(t, 30_001);
  assert.equal(limiter.snapshot().usedRequests, 0);
  await advance(t, 9_999);
  const admission = await pending;
  assert.equal(admission.waitedMs, 70_000);
  assert.ok(admission.waitReasons.tokens >= 60_000);
  assert.ok(admission.waitReasons.cooldown >= 10_000);
  assert.equal(limiter.snapshot().inFlight, 1);
  admission.release();
});

test("waiting before any admission is counted as cooldown, and oversize input never queues", async (t) => {
  const limiter = fixture(t);
  limiter.blockedUntil = Date.now() + 60_000;
  await assert.rejects(limiter.admit(101), /exceeds local TPM ceiling/);
  const pending = limiter.admit(50);
  await advance(t, 60_000);
  const admission = await pending;
  assert.equal(admission.waitedMs, 60_000);
  assert.deepEqual(admission.waitReasons, { cooldown: 60_000 });
  admission.release();
});

test("HTTP wait diagnostics survive authentication retry without logging content or credentials", async (t) => {
  const logs = [];
  let admissions = 0;
  let releases = 0;
  let dispatches = 0;
  const limiter = {
    async admit() {
      admissions++;
      return { waitedMs: admissions === 1 ? 23 : 7,
        waitReasons: admissions === 1 ? { tokens: 23 } : { pacing: 7 },
        release() { releases++; } };
    },
    observeResponse() {},
    snapshot() { return { tokensPerMinute: 100, requestsPerMinute: 60, queuedRequests: 0 }; },
  };
  const server = createGatewayServer({ deployment: TEST_DEPLOYMENT,
    localToken: "private-fixture-token", limiters: new Map([["FW-GLM-5.3", limiter]]),
    log(event, details) { logs.push({ event, ...details }); },
    async getAzureCredential() { return { token: "private-upstream-token", expiresAt: Date.now() + 3_600_000 }; },
    async fetch() { return new Response("fixture-result", { status: ++dispatches === 1 ? 401 : 200 }); },
  });
  t.after(() => new Promise((resolve) => server.close(resolve)));
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const response = await fetch(`http://127.0.0.1:${server.address().port}/azure-foundry/v1/chat/completions`, {
    method: "POST", headers: { authorization: "Bearer private-fixture-token", "content-type": "application/json" },
    body: JSON.stringify({ model: "FW-GLM-5.3", messages: [{ role: "user", content: "PRIVATE-PROMPT" }] }),
  });
  assert.equal(response.status, 200);
  await response.text();
  assert.equal(response.headers.get("x-prime-rate-wait-ms"), "30");
  assert.equal(response.headers.get("x-prime-rate-wait-reasons"), "tokens,pacing");
  const request = logs.find((record) => record.event === "request");
  assert.equal(request.waitedMs, 30);
  assert.deepEqual(request.rateWaitReasonsMs, { tokens: 23, pacing: 7 });
  assert.equal(request.limiterAtResponse.queuedRequests, 0);
  assert.equal(admissions, 2);
  assert.equal(releases, 2);
  assert.doesNotMatch(JSON.stringify(logs), /PRIVATE-PROMPT|private-fixture-token|private-upstream-token/);
});
