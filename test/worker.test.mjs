import assert from "node:assert/strict";
import test from "node:test";

import { handleRequest } from "../worker/index.mjs";

function createBridge(response = new Response("ok\n", { status: 202 })) {
  const requests = [];
  return {
    requests,
    binding: {
      async fetch(request) {
        requests.push(request);
        return response;
      }
    }
  };
}

test("the Worker forwards a GitHub webhook without changing its body or headers", async () => {
  const bridge = createBridge();
  const body = '{"action":"completed"}';
  const request = new Request(
    "https://codex-github-bridge.example.workers.dev/github/webhook?source=github",
    {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-github-delivery": "delivery-1",
        "x-github-event": "workflow_run",
        "x-hub-signature-256": "sha256=signature"
      },
      body
    }
  );

  const response = await handleRequest(request, { BRIDGE: bridge.binding });

  assert.equal(response.status, 202);
  assert.equal(bridge.requests.length, 1);
  const forwarded = bridge.requests[0];
  assert.equal(forwarded.url, "http://localhost/github/webhook?source=github");
  assert.equal(forwarded.method, "POST");
  assert.equal(forwarded.headers.get("x-github-delivery"), "delivery-1");
  assert.equal(forwarded.headers.get("x-github-event"), "workflow_run");
  assert.equal(forwarded.headers.get("x-hub-signature-256"), "sha256=signature");
  assert.equal(await forwarded.text(), body);
});

test("the Worker forwards health checks to the local bridge", async () => {
  const bridge = createBridge(new Response("ok\n"));

  const response = await handleRequest(
    new Request("https://codex-github-bridge.example.workers.dev/healthz"),
    { BRIDGE: bridge.binding }
  );

  assert.equal(response.status, 200);
  assert.equal(await response.text(), "ok\n");
  assert.equal(bridge.requests[0].url, "http://localhost/healthz");
});

test("the Worker rejects all other routes without contacting the bridge", async () => {
  const bridge = createBridge();

  const response = await handleRequest(
    new Request("https://codex-github-bridge.example.workers.dev/"),
    { BRIDGE: bridge.binding }
  );

  assert.equal(response.status, 404);
  assert.deepEqual(await response.json(), { error: "not found" });
  assert.equal(response.headers.get("cache-control"), "no-store");
  assert.equal(bridge.requests.length, 0);
});

test("the Worker rejects paths that only start with an allowed route", async () => {
  const bridge = createBridge();

  for (const [method, path] of [
    ["POST", "/github/webhook/extra"],
    ["GET", "/healthz/debug"]
  ]) {
    const response = await handleRequest(
      new Request(`https://codex-github-bridge.example.workers.dev${path}`, { method }),
      { BRIDGE: bridge.binding }
    );

    assert.equal(response.status, 404);
  }

  assert.equal(bridge.requests.length, 0);
});

test("the Worker returns 502 when the local bridge is unavailable", async () => {
  const response = await handleRequest(
    new Request("https://codex-github-bridge.example.workers.dev/github/webhook", {
      method: "POST",
      body: "{}"
    }),
    {
      BRIDGE: {
        async fetch() {
          throw new Error("connection refused");
        }
      }
    }
  );

  assert.equal(response.status, 502);
  assert.deepEqual(await response.json(), { error: "bridge unavailable" });
  assert.equal(response.headers.get("cache-control"), "no-store");
});
