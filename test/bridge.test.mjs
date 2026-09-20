import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { promisify } from "node:util";

import { createWebhookServer, handleDelivery, linkPullRequest } from "../index.mjs";

const execFileAsync = promisify(execFile);

test("a signed failed CI run resumes the Codex session linked to its PR", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "codex-github-bridge-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const stateFile = join(directory, "state.json");
  await writeFile(
    stateFile,
    JSON.stringify({
      links: {
        "WashizuRyo/menu#27": {
          threadId: "thread-27",
          cwd: "/workspace/menu"
        }
      },
      deliveries: []
    })
  );

  const body = Buffer.from(
    JSON.stringify({
      action: "completed",
      repository: { full_name: "WashizuRyo/menu" },
      workflow_run: {
        id: 1234,
        conclusion: "failure",
        html_url: "https://github.com/WashizuRyo/menu/actions/runs/1234",
        pull_requests: [{ number: 27 }]
      }
    })
  );
  const secret = "webhook-secret";
  const signature = `sha256=${createHmac("sha256", secret).update(body).digest("hex")}`;
  const resumed = [];

  const result = await handleDelivery(
    {
      event: "workflow_run",
      deliveryId: "delivery-1",
      signature,
      body
    },
    {
      secret,
      stateFile,
      resumeCodex: async (link, prompt) => resumed.push({ link, prompt })
    }
  );

  assert.equal(result.status, "processed");
  assert.equal(resumed.length, 1);
  assert.equal(resumed[0].link.threadId, "thread-27");
  assert.equal(resumed[0].link.cwd, "/workspace/menu");
  assert.match(resumed[0].prompt, /1234/);
  assert.deepEqual(JSON.parse(await readFile(stateFile, "utf8")).deliveries, ["delivery-1"]);
});

test("a submitted CodeRabbit review with inline comments resumes the linked session", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "codex-github-bridge-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const stateFile = join(directory, "state.json");
  await writeFile(
    stateFile,
    JSON.stringify({
      links: {
        "WashizuRyo/menu#27": { threadId: "thread-27", cwd: "/workspace/menu" }
      },
      deliveries: []
    })
  );
  const body = Buffer.from(
    JSON.stringify({
      action: "submitted",
      repository: { full_name: "WashizuRyo/menu" },
      pull_request: { number: 27, html_url: "https://github.com/WashizuRyo/menu/pull/27" },
      review: { id: 5254559716, user: { login: "coderabbitai[bot]" } }
    })
  );
  const secret = "webhook-secret";
  const signature = `sha256=${createHmac("sha256", secret).update(body).digest("hex")}`;
  const resumed = [];
  const requestedReviews = [];

  const result = await handleDelivery(
    {
      event: "pull_request_review",
      deliveryId: "delivery-2",
      signature,
      body
    },
    {
      secret,
      stateFile,
      listReviewComments: async (review) => {
        requestedReviews.push(review);
        return [{ id: 4052192293 }];
      },
      resumeCodex: async (link, prompt) => resumed.push({ link, prompt })
    }
  );

  assert.equal(result.status, "processed");
  assert.deepEqual(requestedReviews, [
    { repository: "WashizuRyo/menu", pullNumber: 27, reviewId: 5254559716 }
  ]);
  assert.equal(resumed[0].link.threadId, "thread-27");
  assert.match(resumed[0].prompt, /CodeRabbit review 5254559716/);
});

test("a CodeRabbit review without inline comments is ignored", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "codex-github-bridge-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const stateFile = join(directory, "state.json");
  await writeFile(
    stateFile,
    JSON.stringify({
      links: {
        "WashizuRyo/menu#27": { threadId: "thread-27", cwd: "/workspace/menu" }
      },
      deliveries: []
    })
  );
  const body = Buffer.from(
    JSON.stringify({
      action: "submitted",
      repository: { full_name: "WashizuRyo/menu" },
      pull_request: { number: 27, html_url: "https://github.com/WashizuRyo/menu/pull/27" },
      review: { id: 99, user: { login: "coderabbitai[bot]" } }
    })
  );
  const secret = "webhook-secret";
  let resumeCount = 0;

  const result = await handleDelivery(
    {
      event: "pull_request_review",
      deliveryId: "delivery-no-comments",
      signature: `sha256=${createHmac("sha256", secret).update(body).digest("hex")}`,
      body
    },
    {
      secret,
      stateFile,
      listReviewComments: async () => [],
      resumeCodex: async () => resumeCount++
    }
  );

  assert.equal(result.status, "ignored");
  assert.equal(resumeCount, 0);
});

test("an invalid webhook signature is rejected", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "codex-github-bridge-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  let resumeCount = 0;

  const result = await handleDelivery(
    {
      event: "workflow_run",
      deliveryId: "delivery-invalid",
      signature: "sha256=invalid",
      body: Buffer.from("{}")
    },
    {
      secret: "webhook-secret",
      stateFile: join(directory, "state.json"),
      resumeCodex: async () => resumeCount++
    }
  );

  assert.equal(result.status, "rejected");
  assert.equal(resumeCount, 0);
});

test("the same GitHub delivery is processed only once", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "codex-github-bridge-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const stateFile = join(directory, "state.json");
  await writeFile(
    stateFile,
    JSON.stringify({
      links: {
        "WashizuRyo/menu#27": { threadId: "thread-27", cwd: "/workspace/menu" }
      },
      deliveries: []
    })
  );
  const body = Buffer.from(
    JSON.stringify({
      action: "completed",
      repository: { full_name: "WashizuRyo/menu" },
      workflow_run: {
        id: 1234,
        conclusion: "failure",
        html_url: "https://github.com/WashizuRyo/menu/actions/runs/1234",
        pull_requests: [{ number: 27 }]
      }
    })
  );
  const secret = "webhook-secret";
  const delivery = {
    event: "workflow_run",
    deliveryId: "same-delivery",
    signature: `sha256=${createHmac("sha256", secret).update(body).digest("hex")}`,
    body
  };
  let resumeCount = 0;
  const options = {
    secret,
    stateFile,
    resumeCodex: async () => resumeCount++
  };

  assert.equal((await handleDelivery(delivery, options)).status, "processed");
  assert.equal((await handleDelivery(delivery, options)).status, "ignored");
  assert.equal(resumeCount, 1);
});

test("a failed Codex resume leaves the GitHub delivery retryable", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "codex-github-bridge-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const stateFile = join(directory, "state.json");
  await writeFile(
    stateFile,
    JSON.stringify({
      links: {
        "WashizuRyo/menu#27": { threadId: "thread-27", cwd: "/workspace/menu" }
      },
      deliveries: []
    })
  );
  const body = Buffer.from(
    JSON.stringify({
      action: "completed",
      repository: { full_name: "WashizuRyo/menu" },
      workflow_run: {
        id: 1234,
        conclusion: "failure",
        html_url: "https://github.com/WashizuRyo/menu/actions/runs/1234",
        pull_requests: [{ number: 27 }]
      }
    })
  );
  const secret = "webhook-secret";

  await assert.rejects(
    handleDelivery(
      {
        event: "workflow_run",
        deliveryId: "delivery-retryable",
        signature: `sha256=${createHmac("sha256", secret).update(body).digest("hex")}`,
        body
      },
      {
        secret,
        stateFile,
        resumeCodex: async () => {
          throw new Error("Codex failed");
        }
      }
    ),
    /Codex failed/
  );

  assert.deepEqual(JSON.parse(await readFile(stateFile, "utf8")).deliveries, []);
});

test("link stores the current Codex session for a GitHub pull request", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "codex-github-bridge-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const stateFile = join(directory, "state.json");

  const result = await linkPullRequest("https://github.com/WashizuRyo/menu/pull/27", {
    stateFile,
    threadId: "thread-27",
    cwd: "/workspace/menu"
  });

  assert.equal(result.key, "WashizuRyo/menu#27");
  assert.deepEqual(JSON.parse(await readFile(stateFile, "utf8")), {
    links: {
      "WashizuRyo/menu#27": { threadId: "thread-27", cwd: "/workspace/menu" }
    },
    deliveries: []
  });
});

test("the webhook server accepts a signed GitHub delivery", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "codex-github-bridge-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const stateFile = join(directory, "state.json");
  await writeFile(
    stateFile,
    JSON.stringify({
      links: {
        "WashizuRyo/menu#27": { threadId: "thread-27", cwd: "/workspace/menu" }
      },
      deliveries: []
    })
  );
  const body = JSON.stringify({
    action: "completed",
    repository: { full_name: "WashizuRyo/menu" },
    workflow_run: {
      id: 1234,
      conclusion: "failure",
      html_url: "https://github.com/WashizuRyo/menu/actions/runs/1234",
      pull_requests: [{ number: 27 }]
    }
  });
  const secret = "webhook-secret";
  let resumeCount = 0;
  const server = createWebhookServer({
    secret,
    stateFile,
    resumeCodex: async () => resumeCount++
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => server.close());
  const { port } = server.address();

  const response = await fetch(`http://127.0.0.1:${port}/github/webhook`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-github-event": "workflow_run",
      "x-github-delivery": "delivery-http",
      "x-hub-signature-256": `sha256=${createHmac("sha256", secret).update(body).digest("hex")}`
    },
    body
  });

  assert.equal(response.status, 202);
  assert.equal(resumeCount, 1);
});

test("bridge link uses CODEX_THREAD_ID and the current directory", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "codex-github-bridge-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const stateFile = join(directory, "state.json");

  const { stdout } = await execFileAsync(
    process.execPath,
    [join(process.cwd(), "index.mjs"), "link", "https://github.com/WashizuRyo/menu/pull/27"],
    {
      cwd: directory,
      env: {
        ...process.env,
        CODEX_THREAD_ID: "thread-from-cli",
        BRIDGE_STATE_FILE: stateFile
      }
    }
  );

  assert.match(stdout, /WashizuRyo\/menu#27/);
  assert.deepEqual(JSON.parse(await readFile(stateFile, "utf8")).links, {
    "WashizuRyo/menu#27": { threadId: "thread-from-cli", cwd: await realpath(directory) }
  });
});

test("bridge link works when invoked through an npm-style symlink", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "codex-github-bridge-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const stateFile = join(directory, "state.json");
  const executable = join(directory, "bridge");
  await symlink(join(process.cwd(), "index.mjs"), executable);

  const { stdout } = await execFileAsync(
    executable,
    ["link", "https://github.com/WashizuRyo/menu/pull/27"],
    {
      cwd: directory,
      env: {
        ...process.env,
        CODEX_THREAD_ID: "thread-from-symlink",
        BRIDGE_STATE_FILE: stateFile
      }
    }
  );

  assert.match(stdout, /WashizuRyo\/menu#27/);
  assert.equal(
    JSON.parse(await readFile(stateFile, "utf8")).links["WashizuRyo/menu#27"].threadId,
    "thread-from-symlink"
  );
});
