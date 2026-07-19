#!/usr/bin/env node

import { timingSafeEqual } from "node:crypto";
import { readFileSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { pathToFileURL } from "node:url";
import { addrFromPriv, isValidPriv } from "@inversealtruism/csd-crypto";
import { CsdClient, verifyInputValues } from "@inversealtruism/csd-client";
import { buildSendVerified } from "@inversealtruism/csd-tx";

const MAX_BODY_BYTES = 1024 * 1024;

function required(name, env) {
  const value = String(env[name] ?? "").trim();
  if (!value) throw new Error(`${name} is required`);
  return value;
}

function positiveSafeInteger(value, name) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`${name} must be a positive safe integer`);
  }
  return parsed;
}

export function loadSettings(env = process.env) {
  const token = required("CSD_POOL_SIGNER_TOKEN", env);
  if (token.length < 32) throw new Error("CSD_POOL_SIGNER_TOKEN must be at least 32 characters");
  const keyPath = required("CSD_POOL_SIGNER_PRIVATE_KEY_FILE", env);
  const mode = statSync(keyPath).mode & 0o777;
  if ((mode & 0o077) !== 0) throw new Error("signer private key file must have mode 0600 or stricter");
  const privateKey = readFileSync(keyPath, "utf8").trim();
  if (!isValidPriv(privateKey)) throw new Error("signer private key file does not contain a valid 32-byte CSD key");
  const walletAddress = addrFromPriv(privateKey).toLowerCase();
  const expected = String(env.CSD_POOL_SIGNER_WALLET_ADDRESS ?? "").trim().toLowerCase();
  if (expected && expected.replace(/^0x/, "") !== walletAddress.replace(/^0x/, "")) {
    throw new Error("CSD_POOL_SIGNER_WALLET_ADDRESS does not match the private key");
  }
  const listen = String(env.CSD_POOL_SIGNER_LISTEN ?? "127.0.0.1:8890");
  const separator = listen.lastIndexOf(":");
  if (separator < 1) throw new Error("CSD_POOL_SIGNER_LISTEN must be host:port");
  const host = listen.slice(0, separator);
  const port = positiveSafeInteger(listen.slice(separator + 1), "signer listen port");
  if (port > 65535) throw new Error("signer listen port is out of range");
  return {
    token,
    privateKey,
    walletAddress,
    rpcUrl: required("CSD_POOL_SIGNER_NODE_URL", env).replace(/\/+$/, ""),
    fee: positiveSafeInteger(env.CSD_POOL_SIGNER_FEE_BASE_UNITS ?? "200000", "signer fee"),
    maxBatch: positiveSafeInteger(env.CSD_POOL_SIGNER_MAX_BATCH_BASE_UNITS ?? "100000000000", "signer max batch"),
    host,
    port,
  };
}

function authorized(header, expected) {
  const actual = String(header ?? "").replace(/^Bearer /, "");
  const left = Buffer.from(actual);
  const right = Buffer.from(expected);
  return left.length === right.length && timingSafeEqual(left, right);
}

function json(res, status, body) {
  const encoded = JSON.stringify(body);
  res.writeHead(status, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(encoded),
    "cache-control": "no-store",
    "x-content-type-options": "nosniff",
  });
  res.end(encoded);
}

async function bodyJson(req) {
  const chunks = [];
  let size = 0;
  for await (const chunk of req) {
    size += chunk.length;
    if (size > MAX_BODY_BYTES) throw new Error("request body too large");
    chunks.push(chunk);
  }
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

function validatePayout(body, maxBatch) {
  if (!body || typeof body !== "object" || typeof body.batch_id !== "string" || !body.batch_id.trim()) {
    throw new Error("batch_id is required");
  }
  if (!Array.isArray(body.outputs) || body.outputs.length < 1 || body.outputs.length > 512) {
    throw new Error("outputs must contain 1..512 entries");
  }
  const outputs = body.outputs.map((output) => {
    const address = `0x${String(output.address ?? "").trim().replace(/^0x/, "")}`;
    if (!/^0x[0-9a-fA-F]{40}$/.test(address)) throw new Error("output address must be 20-byte hex");
    return { to: address, value: positiveSafeInteger(output.amount_base_units, "output amount") };
  });
  const total = outputs.reduce((sum, output) => {
    const next = sum + output.value;
    if (!Number.isSafeInteger(next)) throw new Error("output total exceeds safe integer range");
    return next;
  }, 0);
  if (total !== positiveSafeInteger(body.total_base_units, "total_base_units")) {
    throw new Error("output total does not match total_base_units");
  }
  if (total > maxBatch) throw new Error("payout exceeds signer max batch limit");
  return outputs;
}

export function createSigner(settings, dependencies = {}) {
  const client = dependencies.client ?? new CsdClient({ baseUrl: settings.rpcUrl, timeoutMs: 10000 });
  const build = dependencies.build ?? buildSendVerified;
  let signing = Promise.resolve();

  const handler = async (req, res) => {
    if (req.method === "GET" && req.url === "/health") {
      return json(res, 200, {
        ok: true,
        service: "csd-pool-wallet-signer",
        mode: "wallet-csd-sdk",
        wallet_address: settings.walletAddress,
      });
    }
    if (req.method !== "POST" || req.url !== "/api/payout/sign") {
      return json(res, 404, { error: { code: "not_found", message: "not found" } });
    }
    if (!authorized(req.headers.authorization, settings.token)) {
      return json(res, 401, { error: { code: "unauthorized", message: "invalid signer token" } });
    }
    try {
      const body = await bodyJson(req);
      const outputs = validatePayout(body, settings.maxBatch);
      const execute = async () => {
        const response = await client.utxos(settings.walletAddress, { available: true });
        if (!response?.ok || !Array.isArray(response.utxos)) throw new Error("failed to load spendable signer UTXOs");
        const built = await build({
          outputs,
          fee: settings.fee,
          utxos: response.utxos,
          priv: settings.privateKey,
          verify: (inputs) => verifyInputValues(client, inputs),
        });
        if (!built.ok) throw new Error(built.error ?? "official CSD transaction build failed");
        return { node_tx: built.nodeJson, txid: built.txid };
      };
      const result = signing.then(execute, execute);
      signing = result.then(() => undefined, () => undefined);
      return json(res, 200, await result);
    } catch (error) {
      return json(res, 400, {
        error: { code: "signing_failed", message: String(error?.message ?? error) },
      });
    }
  };
  return createServer((req, res) => void handler(req, res));
}

export async function main(env = process.env) {
  const settings = loadSettings(env);
  const server = createSigner(settings);
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(settings.port, settings.host, resolve);
  });
  process.stdout.write(`csd wallet signer listening on ${settings.host}:${settings.port}\n`);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    process.stderr.write(`csd wallet signer startup failed: ${error.message}\n`);
    process.exitCode = 1;
  });
}
