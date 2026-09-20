#!/usr/bin/env node

import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, unlink, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { homedir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

const defaultRelayDir = join(
  homedir(),
  "Library",
  "Application Support",
  "codex-github-bridge",
  "relays"
);

export async function startAcpRelay({ command, relayDir = defaultRelayDir }) {
  if (command.length === 0) throw new Error("ACP agent command is required");
  await mkdir(relayDir, { recursive: true, mode: 0o700 });
  const socketPath = join(relayDir, `relay-${process.pid}.sock`);
  await unlink(socketPath).catch((error) => {
    if (error.code !== "ENOENT") throw error;
  });

  const child = spawn(command[0], command.slice(1), {
    env: process.env,
    stdio: ["pipe", "pipe", "inherit"]
  });
  const sessions = new Set();
  const bridgeRequestIds = new Set();
  const registerSession = async (threadId) => {
    if (!/^[0-9A-Za-z_-]+$/.test(threadId)) throw new Error("Invalid ACP session ID");
    sessions.add(threadId);
    await writeFile(
      join(relayDir, `${threadId}.json`),
      `${JSON.stringify({ socketPath })}\n`,
      { mode: 0o600 }
    );
  };

  createInterface({ input: process.stdin }).on("line", async (line) => {
    const message = JSON.parse(line);
    if (message.params?.sessionId) await registerSession(message.params.sessionId);
    child.stdin.write(`${line}\n`);
  });
  createInterface({ input: child.stdout }).on("line", (line) => {
    const message = JSON.parse(line);
    if (bridgeRequestIds.delete(message.id)) return;
    process.stdout.write(`${line}\n`);
  });

  const server = createServer((connection) => {
    createInterface({ input: connection }).once("line", async (line) => {
      try {
        const request = JSON.parse(line);
        const id = `bridge:${randomUUID()}`;
        bridgeRequestIds.add(id);
        process.stdout.write(
          `${JSON.stringify({
            jsonrpc: "2.0",
            method: "session/update",
            params: {
              sessionId: request.sessionId,
              update: {
                sessionUpdate: "user_message_chunk",
                content: { type: "text", text: request.prompt }
              }
            }
          })}\n`
        );
        child.stdin.write(
          `${JSON.stringify({
            jsonrpc: "2.0",
            id,
            method: "session/prompt",
            params: {
              sessionId: request.sessionId,
              prompt: [{ type: "text", text: request.prompt }]
            }
          })}\n`
        );
        connection.end(`${JSON.stringify({ status: "accepted" })}\n`);
      } catch (error) {
        connection.end(`${JSON.stringify({ error: error.message })}\n`);
      }
    });
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(socketPath, resolve);
  });

  const close = async () => {
    server.close();
    child.kill();
    await unlink(socketPath).catch(() => {});
    await Promise.all(
      [...sessions].map(async (threadId) => {
        const path = join(relayDir, `${threadId}.json`);
        try {
          const mapping = JSON.parse(await readFile(path, "utf8"));
          if (mapping.socketPath === socketPath) await unlink(path);
        } catch {}
      })
    );
  };
  return { child, close, socketPath };
}

async function main() {
  const relay = await startAcpRelay({
    command: process.argv.slice(2),
    relayDir: process.env.BRIDGE_RELAY_DIR || defaultRelayDir,
  });
  const shutdown = () => relay.close().finally(() => process.exit());
  process.once("SIGINT", shutdown);
  process.once("SIGTERM", shutdown);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
