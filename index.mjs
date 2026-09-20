#!/usr/bin/env node

import { createHmac, timingSafeEqual } from "node:crypto";
import { spawn, execFile } from "node:child_process";
import { existsSync, realpathSync } from "node:fs";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const defaultStateFile = join(
  homedir(),
  "Library",
  "Application Support",
  "codex-github-bridge",
  "state.json"
);

function validSignature(secret, body, signature) {
  if (!secret || !signature) return false;
  const expected = Buffer.from(
    `sha256=${createHmac("sha256", secret).update(body).digest("hex")}`
  );
  const actual = Buffer.from(signature);
  return expected.length === actual.length && timingSafeEqual(expected, actual);
}

async function readState(path) {
  try {
    return JSON.parse(await readFile(path, "utf8"));
  } catch (error) {
    if (error.code === "ENOENT") return { links: {}, deliveries: [] };
    throw error;
  }
}

async function writeState(path, state) {
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, `${JSON.stringify(state, null, 2)}\n`, { mode: 0o600 });
}

export async function linkPullRequest(pullRequestUrl, options) {
  if (!options.threadId) throw new Error("CODEX_THREAD_ID is required");
  const url = new URL(pullRequestUrl);
  const match = url.pathname.match(/^\/([^/]+)\/([^/]+)\/pull\/(\d+)\/?$/);
  if (url.hostname !== "github.com" || !match) {
    throw new Error("Expected a GitHub pull request URL");
  }
  const key = `${match[1]}/${match[2]}#${match[3]}`;
  const state = await readState(options.stateFile);
  state.links[key] = { threadId: options.threadId, cwd: options.cwd };
  await writeState(options.stateFile, state);
  return { key };
}

export async function handleDelivery(delivery, options) {
  if (!validSignature(options.secret, delivery.body, delivery.signature)) {
    return { status: "rejected", reason: "invalid signature" };
  }

  const payload = JSON.parse(delivery.body.toString("utf8"));
  const state = await readState(options.stateFile);
  if (state.deliveries.includes(delivery.deliveryId)) {
    return { status: "ignored", reason: "duplicate delivery" };
  }
  let pullNumber;
  let prompt;

  if (delivery.event === "workflow_run") {
    const run = payload.workflow_run;
    const pullRequest = run?.pull_requests?.[0];
    if (payload.action !== "completed" || run?.conclusion !== "failure" || !pullRequest) {
      return { status: "ignored" };
    }
    pullNumber = pullRequest.number;
    prompt = `GitHub Actions run ${run.id} failed for ${payload.repository.full_name}#${pullNumber}. Inspect ${run.html_url}, fix the failure, run the relevant tests, commit, and push the fix.`;
  } else if (delivery.event === "pull_request_review") {
    const review = payload.review;
    if (
      payload.action !== "submitted" ||
      !["coderabbitai", "coderabbitai[bot]"].includes(review?.user?.login)
    ) {
      return { status: "ignored" };
    }
    pullNumber = payload.pull_request?.number;
    const comments = await options.listReviewComments({
      repository: payload.repository.full_name,
      pullNumber,
      reviewId: review.id
    });
    if (comments.length === 0) return { status: "ignored", reason: "review has no comments" };
    prompt = `CodeRabbit review ${review.id} has inline comments on ${payload.pull_request.html_url}. Verify the comments against the current code, fix valid findings, run the relevant tests, commit, and push the fix.`;
  } else {
    return { status: "ignored" };
  }

  const key = `${payload.repository.full_name}#${pullNumber}`;
  const link = state.links[key];
  if (!link) return { status: "ignored", reason: "PR is not linked" };

  state.deliveries = [...state.deliveries.slice(-99), delivery.deliveryId];
  await writeState(options.stateFile, state);
  await options.resumeCodex(link, prompt);
  return { status: "processed" };
}

export function createWebhookServer(options) {
  return createServer(async (request, response) => {
    if (request.method === "GET" && request.url === "/healthz") {
      response.writeHead(200).end("ok\n");
      return;
    }
    if (request.method !== "POST" || request.url !== "/github/webhook") {
      response.writeHead(404).end("not found\n");
      return;
    }

    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    try {
      const result = await handleDelivery(
        {
          event: request.headers["x-github-event"],
          deliveryId: request.headers["x-github-delivery"],
          signature: request.headers["x-hub-signature-256"],
          body: Buffer.concat(chunks)
        },
        options
      );
      const statusCode = result.status === "rejected" ? 401 : 202;
      response.writeHead(statusCode, { "content-type": "application/json" });
      response.end(`${JSON.stringify(result)}\n`);
    } catch (error) {
      options.log?.(error);
      response.writeHead(500).end("internal error\n");
    }
  });
}

async function listReviewComments({ repository, pullNumber, reviewId }) {
  const endpoint = `repos/${repository}/pulls/${pullNumber}/reviews/${reviewId}/comments?per_page=1`;
  const { stdout } = await execFileAsync("gh", ["api", endpoint]);
  return JSON.parse(stdout);
}

function resumeCodex(codexBin) {
  return async (link, prompt) => {
    const child = spawn(codexBin, ["exec", "resume", link.threadId, prompt], {
      cwd: link.cwd,
      stdio: "inherit"
    });
    child.on("error", (error) => console.error("Failed to start Codex:", error.message));
  };
}

async function main() {
  const [, , command, argument] = process.argv;
  const stateFile = process.env.BRIDGE_STATE_FILE || defaultStateFile;

  if (command === "link") {
    if (!argument) throw new Error("Usage: bridge link <GitHub PR URL>");
    const result = await linkPullRequest(argument, {
      stateFile,
      threadId: process.env.CODEX_THREAD_ID,
      cwd: process.cwd()
    });
    console.log(`Linked ${result.key} to ${process.env.CODEX_THREAD_ID}`);
    return;
  }

  if (command === "serve") {
    if (!process.env.WEBHOOK_SECRET) throw new Error("WEBHOOK_SECRET is required");
    const appCodex = "/Applications/ChatGPT.app/Contents/Resources/codex";
    const codexBin = process.env.CODEX_BIN || (existsSync(appCodex) ? appCodex : "codex");
    const port = Number(process.env.PORT || 8787);
    const server = createWebhookServer({
      secret: process.env.WEBHOOK_SECRET,
      stateFile,
      listReviewComments,
      resumeCodex: resumeCodex(codexBin),
      log: console.error
    });
    server.listen(port, "127.0.0.1", () => {
      console.log(`Listening on http://127.0.0.1:${port}`);
    });
    return;
  }

  throw new Error("Usage: bridge <serve|link> [argument]");
}

const isMain = process.argv[1] && import.meta.url === pathToFileURL(realpathSync(process.argv[1])).href;
if (isMain) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
