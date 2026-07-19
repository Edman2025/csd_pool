import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { addrFromPriv } from "@inversealtruism/csd-crypto";
import { buildSendVerified } from "@inversealtruism/csd-tx";
import { createSigner, loadSettings } from "./signer.mjs";

const dir = mkdtempSync(join(tmpdir(), "csd-wallet-signer-test-"));
after(() => rmSync(dir, { recursive: true, force: true }));
const privateKey = `0x${"01".repeat(32)}`;
const keyPath = join(dir, "wallet.key");
writeFileSync(keyPath, `${privateKey}\n`, { mode: 0o600 });
chmodSync(keyPath, 0o600);

function env(overrides = {}) {
  return {
    CSD_POOL_SIGNER_TOKEN: "0123456789abcdef0123456789abcdef",
    CSD_POOL_SIGNER_PRIVATE_KEY_FILE: keyPath,
    CSD_POOL_SIGNER_NODE_URL: "http://127.0.0.1:8790",
    CSD_POOL_SIGNER_WALLET_ADDRESS: addrFromPriv(privateKey),
    ...overrides,
  };
}

test("loads a restricted key file and binds its wallet", () => {
  const settings = loadSettings(env());
  assert.equal(settings.walletAddress, addrFromPriv(privateKey));
  assert.equal(settings.fee, 200000);
});

test("rejects a wallet address that does not match the key", () => {
  assert.throws(
    () => loadSettings(env({ CSD_POOL_SIGNER_WALLET_ADDRESS: `0x${"ab".repeat(20)}` })),
    /does not match/,
  );
});

test("builds a signed official transaction with the pinned SDK", async () => {
  const sourceTxid = `0x${"11".repeat(32)}`;
  const built = await buildSendVerified({
    outputs: [{ to: `0x${"ab".repeat(20)}`, value: 546 }],
    fee: 200000,
    utxos: [{
      txid: sourceTxid,
      vout: 2,
      value: 1000000,
      confirmations: 10,
      coinbase: false,
    }],
    priv: privateKey,
    verify: async (inputs) => {
      assert.deepEqual(inputs, [{ txid: sourceTxid, vout: 2, value: 1000000 }]);
      return { ok: true, total: 1000000 };
    },
  });
  assert.equal(built.ok, true, built.error);
  assert.match(built.txid, /^(?:0x)?[0-9a-f]{64}$/i);
  assert.equal(built.nodeJson.app, "None");
  assert.equal(built.nodeJson.inputs.length, 1);
  assert.equal(built.nodeJson.inputs[0].prevout.txid.length, 32);
  assert.equal(built.nodeJson.inputs[0].script_sig.length, 99);
  assert.equal(built.nodeJson.outputs.length, 2);
  assert.equal(built.nodeJson.outputs[0].value, 546);
  assert.deepEqual(built.nodeJson.outputs[0].script_pubkey, Array(20).fill(0xab));
});

test("returns official node transaction JSON with exact outputs", async () => {
  const walletAddress = addrFromPriv(privateKey);
  const nodeTx = {
    version: 1,
    inputs: [{ prevout: { txid: Array(32).fill(1), vout: 0 }, script_sig: Array(99).fill(2) }],
    outputs: [{ value: 546, script_pubkey: Array(20).fill(3) }],
    locktime: 0,
    app: "None",
  };
  const settings = loadSettings(env({ CSD_POOL_SIGNER_LISTEN: "127.0.0.1:18991" }));
  const server = createSigner(settings, {
    client: { utxos: async () => ({ ok: true, utxos: [] }) },
    build: async ({ outputs }) => {
      assert.deepEqual(outputs, [{ to: `0x${"ab".repeat(20)}`, value: 546 }]);
      return { ok: true, nodeJson: nodeTx, txid: `0x${"12".repeat(32)}` };
    },
  });
  await new Promise((resolve) => server.listen(18991, "127.0.0.1", resolve));
  try {
    const response = await fetch("http://127.0.0.1:18991/api/payout/sign", {
      method: "POST",
      headers: {
        authorization: `Bearer ${settings.token}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({
        batch_id: "batch-1",
        total_base_units: 546,
        outputs: [{ address: "ab".repeat(20), amount_base_units: 546 }],
      }),
    });
    assert.equal(response.status, 200);
    const body = await response.json();
    assert.deepEqual(body.node_tx, nodeTx);
    assert.equal(body.txid, `0x${"12".repeat(32)}`);
    const health = await (await fetch("http://127.0.0.1:18991/health")).json();
    assert.equal(health.mode, "wallet-csd-sdk");
    assert.equal(health.wallet_address, walletAddress);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});
