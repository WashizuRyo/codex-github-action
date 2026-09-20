import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import test from "node:test";

import { createAcpResumer } from "../index.mjs";

test("a webhook prompt is sent through the ACP connection that owns the session", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "codex-github-bridge-"));
  await rm(directory, { recursive: true, force: true });
  t.after(() => rm(directory, { recursive: true, force: true }));
  const relay = spawn(
    process.execPath,
    [
      join(process.cwd(), "acp-relay.mjs"),
      process.execPath,
      join(process.cwd(), "fixtures/fake-acp-agent.mjs")
    ],
    {
      env: { ...process.env, BRIDGE_RELAY_DIR: directory },
      stdio: ["pipe", "pipe", "inherit"]
    }
  );
  t.after(() => relay.kill());
  const messages = createMessageReader(relay.stdout);

  relay.stdin.write(
    `${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: {} })}\n`
  );
  await messages.next((message) => message.id === 1);
  relay.stdin.write(
    `${JSON.stringify({
      jsonrpc: "2.0",
      id: 2,
      method: "session/prompt",
      params: {
        sessionId: "thread-27",
        prompt: [{ type: "text", text: "Original user prompt" }]
      }
    })}\n`
  );
  await messages.next((message) => message.id === 2);

  const result = await createAcpResumer({ relayDir: directory })(
    { threadId: "thread-27", cwd: "/workspace/menu" },
    "Webhook prompt"
  );
  const update = await messages.next(
    (message) =>
      message.method === "session/update" &&
      message.params?.update?.content?.text === "handled: Webhook prompt"
  );

  assert.deepEqual(result, { status: "accepted" });
  assert.equal(update.params.sessionId, "thread-27");
});

function createMessageReader(stream) {
  const waiting = [];
  const queued = [];
  createInterface({ input: stream }).on("line", (line) => {
    const message = JSON.parse(line);
    const index = waiting.findIndex(({ predicate }) => predicate(message));
    if (index === -1) queued.push(message);
    else waiting.splice(index, 1)[0].resolve(message);
  });
  return {
    next(predicate) {
      const index = queued.findIndex(predicate);
      if (index !== -1) return Promise.resolve(queued.splice(index, 1)[0]);
      return new Promise((resolve) => waiting.push({ predicate, resolve }));
    }
  };
}
