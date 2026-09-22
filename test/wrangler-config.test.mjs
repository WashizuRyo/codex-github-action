import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { getWranglerArgs, renderWranglerConfig } from "../scripts/wrangler.mjs";

const template = await readFile(
  new URL("../wrangler.example.jsonc", import.meta.url),
  "utf8"
);

test("Wrangler config is generated from self-hosted environment values", () => {
  const rendered = renderWranglerConfig(template, {
    CLOUDFLARE_WORKER_NAME: "my-bridge",
    CLOUDFLARE_VPC_SERVICE_ID: "00000000-1111-2222-3333-444444444444"
  });

  assert.match(rendered, /"name": "my-bridge"/);
  assert.match(rendered, /"service_id": "00000000-1111-2222-3333-444444444444"/);
  assert.doesNotMatch(rendered, /__CLOUDFLARE_/);
});

test("Wrangler config generation rejects missing self-hosted values", () => {
  assert.throws(
    () =>
      renderWranglerConfig(template, {
        CLOUDFLARE_WORKER_NAME: "my-bridge",
        CLOUDFLARE_VPC_SERVICE_ID: ""
      }),
    /CLOUDFLARE_VPC_SERVICE_ID must be set/
  );
});

test("Wrangler config generation rejects a malformed VPC Service ID", () => {
  assert.throws(
    () =>
      renderWranglerConfig(template, {
        CLOUDFLARE_WORKER_NAME: "my-bridge",
        CLOUDFLARE_VPC_SERVICE_ID: "not-a-service-id"
      }),
    /CLOUDFLARE_VPC_SERVICE_ID must be a UUID/
  );
});

test("Tunnel command reads its tunnel name from the environment", () => {
  assert.deepEqual(
    getWranglerArgs("tunnel", { CLOUDFLARE_TUNNEL_NAME: "my-tunnel" }),
    ["tunnel", "run", "my-tunnel"]
  );
});
