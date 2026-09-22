#!/usr/bin/env node

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const envFile = join(projectRoot, ".env");
const templateFile = join(projectRoot, "wrangler.example.jsonc");
const generatedFile = join(projectRoot, ".wrangler.generated.jsonc");
const wranglerCli = join(projectRoot, "node_modules", "wrangler", "bin", "wrangler.js");

function required(env, name) {
  const value = env[name]?.trim();
  if (!value) throw new Error(`${name} must be set in .env`);
  return value;
}

function escapeJsonString(value) {
  return JSON.stringify(value).slice(1, -1);
}

export function renderWranglerConfig(template, env) {
  const workerName = required(env, "CLOUDFLARE_WORKER_NAME");
  const serviceId = required(env, "CLOUDFLARE_VPC_SERVICE_ID");

  if (!/^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?$/.test(workerName)) {
    throw new Error(
      "CLOUDFLARE_WORKER_NAME must be a valid workers.dev DNS label"
    );
  }
  if (!/^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/i.test(serviceId)) {
    throw new Error("CLOUDFLARE_VPC_SERVICE_ID must be a UUID");
  }

  return template
    .replaceAll("__CLOUDFLARE_WORKER_NAME__", escapeJsonString(workerName))
    .replaceAll("__CLOUDFLARE_VPC_SERVICE_ID__", escapeJsonString(serviceId));
}

export function getWranglerArgs(command, env) {
  if (command === "tunnel") {
    return ["tunnel", "run", required(env, "CLOUDFLARE_TUNNEL_NAME")];
  }

  const commands = {
    check: ["deploy", "--dry-run"],
    deploy: ["deploy"],
    dev: ["dev"],
    tail: ["tail"]
  };
  const args = commands[command];
  if (!args) {
    throw new Error("Usage: node scripts/wrangler.mjs <check|deploy|dev|tail|tunnel>");
  }
  return [...args, "--config", generatedFile];
}

async function main() {
  if (existsSync(envFile)) process.loadEnvFile(envFile);

  const command = process.argv[2];
  const args = getWranglerArgs(command, process.env);
  if (command !== "tunnel") {
    const template = await readFile(templateFile, "utf8");
    await writeFile(generatedFile, renderWranglerConfig(template, process.env), {
      mode: 0o600
    });
  }

  const child = spawn(process.execPath, [wranglerCli, ...args], {
    cwd: projectRoot,
    stdio: "inherit"
  });
  const exitCode = await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (signal) reject(new Error(`wrangler exited after signal ${signal}`));
      else resolve(code ?? 1);
    });
  });
  process.exitCode = exitCode;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
