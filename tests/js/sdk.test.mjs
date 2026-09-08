// @hiero-ledger/sdk against a running hanvil over HAPI gRPC: the exact sequence the harness's
// chainSigner runs — provision an account, read its balance, transfer, submit a topic message,
// sweep, delete — plus the queries the SDK issues along the way. Boots its own node on random
// ports; HANVIL_BIN overrides the binary.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import {
  AccountBalanceQuery,
  AccountCreateTransaction,
  AccountDeleteTransaction,
  AccountId,
  AccountInfoQuery,
  Client,
  Hbar,
  PrivateKey,
  TopicCreateTransaction,
  TopicInfoQuery,
  TopicMessageSubmitTransaction,
  TransactionRecordQuery,
  TransferTransaction,
} from "@hiero-ledger/sdk";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");

// 0.0.1002, the first ECDSA account. Its id and key are hiero-local-node's, so a client
// configured for local-node needs no change.
const OPERATOR_ID = "0.0.1002";
const OPERATOR_KEY = "0x7f109a9e3b0d8ecfba9cc23a3614433ce0fa7ddcc80f2a8f10b222179a5a80d6";

function binary() {
  if (process.env.HANVIL_BIN) return process.env.HANVIL_BIN;
  const candidates = ["release", "debug"]
    .map((profile) => join(root, "target", profile, "hanvil"))
    .filter((path) => existsSync(path))
    .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  if (candidates.length === 0) {
    throw new Error("no hanvil binary: cargo build first or set HANVIL_BIN");
  }
  return candidates[0];
}

// Boot on random ports and read them off the banner, so the test never needs 50211 to be free.
async function boot() {
  const child = spawn(binary(), ["--port", "0", "--mirror-port", "0", "--grpc-port", "0"], {
    stdio: ["ignore", "pipe", "ignore"],
  });
  const ports = await new Promise((resolve, reject) => {
    let buffered = "";
    const timer = setTimeout(() => reject(new Error("hanvil did not print a banner")), 10_000);
    child.stdout.on("data", (chunk) => {
      buffered += chunk;
      const grpc = buffered.match(/HAPI gRPC {2}127\.0\.0\.1:(\d+)/);
      const mirror = buffered.match(/Mirror {5}http:\/\/127\.0\.0\.1:(\d+)/);
      if (grpc && mirror && buffered.includes("Started in")) {
        clearTimeout(timer);
        resolve({ grpc: Number(grpc[1]), mirror: Number(mirror[1]) });
      }
    });
    child.on("error", reject);
  });
  child.stdout.resume();

  const client = Client.forNetwork({ [`127.0.0.1:${ports.grpc}`]: new AccountId(3) })
    .setOperator(AccountId.fromString(OPERATOR_ID), PrivateKey.fromStringECDSA(OPERATOR_KEY));
  client.setMirrorNetwork([]);
  return {
    client,
    ports,
    async close() {
      client.close();
      child.kill();
      await new Promise((resolve) => child.on("exit", resolve));
    },
  };
}

test("the SDK provisions, funds, reads and deletes an account over HAPI", async () => {
  const node = await boot();
  try {
    const { client } = node;

    // Provision — this is what chainSigner.ts does to get an ephemeral signer.
    const key = PrivateKey.generateECDSA();
    const created = await new AccountCreateTransaction()
      .setECDSAKeyWithAlias(key)
      .setInitialBalance(new Hbar(20))
      .execute(client);
    const receipt = await created.getReceipt(client);
    assert.equal(receipt.status.toString(), "SUCCESS");
    const signer = receipt.accountId;
    assert.ok(signer, "the receipt names the new account");
    assert.ok(signer.num.toNumber() >= 1002, `unexpected id ${signer.toString()}`);

    // Balance — free query, no payment attached.
    const balance = await new AccountBalanceQuery().setAccountId(signer).execute(client);
    assert.equal(balance.hbars.toTinybars().toString(), new Hbar(20).toTinybars().toString());

    // Info — a paid query: the SDK asks the cost, then attaches a payment for it.
    const info = await new AccountInfoQuery().setAccountId(signer).execute(client);
    assert.equal(info.accountId.toString(), signer.toString());
    assert.equal(info.balance.toTinybars().toString(), new Hbar(20).toTinybars().toString());
    assert.match(info.contractAccountId, /^[0-9a-f]{40}$/);

    // Fund it further from the operator.
    const transfer = await new TransferTransaction()
      .addHbarTransfer(client.operatorAccountId, new Hbar(-5))
      .addHbarTransfer(signer, new Hbar(5))
      .execute(client);
    assert.equal((await transfer.getReceipt(client)).status.toString(), "SUCCESS");

    const funded = await new AccountBalanceQuery().setAccountId(signer).execute(client);
    assert.equal(funded.hbars.toTinybars().toString(), new Hbar(25).toTinybars().toString());

    // The record query answers from the same record the receipt came from.
    const record = await new TransactionRecordQuery()
      .setTransactionId(transfer.transactionId)
      .execute(client);
    assert.equal(record.receipt.status.toString(), "SUCCESS");
    assert.ok(record.transfers.length >= 2, "the record itemises the HBAR that moved");

    // Sweep and delete, which is how the harness cleans a signer up.
    const deleted = await new AccountDeleteTransaction()
      .setAccountId(signer)
      .setTransferAccountId(client.operatorAccountId)
      .freezeWith(client)
      .sign(key);
    const submitted = await deleted.execute(client);
    assert.equal((await submitted.getReceipt(client)).status.toString(), "SUCCESS");

    const swept = await new AccountBalanceQuery().setAccountId(signer).execute(client);
    assert.equal(swept.hbars.toTinybars().toString(), "0");
  } finally {
    await node.close();
  }
});

test("a topic accepts messages and the running hash advances", async () => {
  const node = await boot();
  try {
    const { client } = node;
    const created = await new TopicCreateTransaction()
      .setTopicMemo("hanvil test topic")
      .execute(client);
    const topicId = (await created.getReceipt(client)).topicId;
    assert.ok(topicId, "the receipt names the new topic");

    const first = await (await new TopicMessageSubmitTransaction()
      .setTopicId(topicId)
      .setMessage("one")
      .execute(client)).getReceipt(client);
    assert.equal(first.status.toString(), "SUCCESS");
    assert.equal(first.topicSequenceNumber.toNumber(), 1);
    // The SDK's receipt carries no topicRunningHashVersion field; tests/hapi.rs checks the 3 on
    // the wire. Here the 48 bytes are the evidence the hash is a SHA-384.
    assert.equal(first.topicRunningHash.length, 48);

    const second = await (await new TopicMessageSubmitTransaction()
      .setTopicId(topicId)
      .setMessage("two")
      .execute(client)).getReceipt(client);
    assert.equal(second.topicSequenceNumber.toNumber(), 2);
    assert.notEqual(
      Buffer.from(second.topicRunningHash).toString("hex"),
      Buffer.from(first.topicRunningHash).toString("hex"),
    );

    const info = await new TopicInfoQuery().setTopicId(topicId).execute(client);
    assert.equal(info.topicMemo, "hanvil test topic");
    assert.equal(info.sequenceNumber.toNumber(), 2);
  } finally {
    await node.close();
  }
});

test("a transfer that does not balance is refused with the protocol's code", async () => {
  const node = await boot();
  try {
    const { client } = node;
    const receiver = "0.0.1003";
    await assert.rejects(
      async () => {
        const response = await new TransferTransaction()
          .addHbarTransfer(client.operatorAccountId, new Hbar(-1))
          .addHbarTransfer(receiver, new Hbar(1))
          // A third leg with no counterpart: the list no longer sums to zero.
          .addHbarTransfer("0.0.1004", new Hbar(1))
          .execute(client);
        await response.getReceipt(client);
      },
      (error) => {
        assert.match(String(error), /INVALID_ACCOUNT_AMOUNTS/);
        return true;
      },
    );
  } finally {
    await node.close();
  }
});
