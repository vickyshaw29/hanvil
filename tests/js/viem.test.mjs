// viem against a running hanvil: deploy the Counter fixture, write, read, logs, custom error,
// snapshot and revert. Boots its own node on random ports; HANVIL_BIN overrides the binary.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import {
  ContractFunctionRevertedError,
  createPublicClient,
  createWalletClient,
  defineChain,
  http,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");
const fixture = JSON.parse(readFileSync(join(root, "tests", "fixtures", "Counter.json"), "utf8"));

// 0.0.1012, the first alias account of hiero-local-node.
const KEY = "0x105d050185ccb907fba04dd92d8de9e32c18305e097ab41dadda21489a211524";

// The most recently built of target/release and target/debug, so a stale release binary never
// shadows the build under test.
function binary() {
  if (process.env.HANVIL_BIN) return process.env.HANVIL_BIN;
  const candidates = ["release", "debug"]
    .map((profile) => join(root, "target", profile, "hanvil"))
    .filter((path) => existsSync(path))
    .sort((a, b) => statSync(b).mtimeMs - statSync(a).mtimeMs);
  if (candidates.length === 0) throw new Error("no hanvil binary: cargo build first or set HANVIL_BIN");
  return candidates[0];
}

async function boot() {
  const child = spawn(binary(), ["--port", "0", "--mirror-port", "0", "--grpc-port", "0"], {
    stdio: ["ignore", "pipe", "ignore"],
  });
  const url = await new Promise((resolve, reject) => {
    let buffer = "";
    const timer = setTimeout(() => reject(new Error("hanvil did not print its banner in 10 s")), 10_000);
    child.stdout.on("data", (chunk) => {
      buffer += chunk.toString();
      const match = buffer.match(/JSON-RPC\s+(http:\/\/127\.0\.0\.1:\d+)/);
      if (match && buffer.includes("Started in")) {
        clearTimeout(timer);
        resolve(match[1]);
      }
    });
    child.on("exit", (code) => reject(new Error(`hanvil exited with ${code}`)));
  });
  return { child, url };
}

const chain = defineChain({
  id: 298,
  name: "hanvil",
  nativeCurrency: { name: "HBAR", symbol: "HBAR", decimals: 18 },
  rpcUrls: { default: { http: [] } },
});

test("viem deploys, writes, reads, filters logs, decodes a custom error, snapshots and reverts", async (t) => {
  const { child, url } = await boot();
  t.after(() => child.kill());
  const account = privateKeyToAccount(KEY);
  const transport = http(url);
  const publicClient = createPublicClient({ chain, transport });
  const wallet = createWalletClient({ account, chain, transport });

  assert.equal(await publicClient.getChainId(), 298);
  assert.equal(await publicClient.getBlockNumber(), 0n);
  const balance = await publicClient.getBalance({ address: account.address });
  assert.equal(balance, 10_000n * 10n ** 18n, "10,000 HBAR rendered with 18 decimals");

  // Deploy with viem's default EIP-1559 transaction.
  const deployHash = await wallet.deployContract({ abi: fixture.abi, bytecode: fixture.bytecode });
  const deployReceipt = await publicClient.waitForTransactionReceipt({ hash: deployHash });
  assert.equal(deployReceipt.status, "success");
  assert.equal(deployReceipt.blockNumber, 1n);
  const address = deployReceipt.contractAddress;
  assert.ok(address, "contractAddress on the receipt");
  assert.equal(await publicClient.getCode({ address }), fixture.deployedBytecode);

  // Write, then read.
  const counter = { address, abi: fixture.abi };
  const incrementHash = await wallet.writeContract({ ...counter, functionName: "increment" });
  const incrementReceipt = await publicClient.waitForTransactionReceipt({ hash: incrementHash });
  assert.equal(incrementReceipt.status, "success");
  assert.equal(incrementReceipt.logs.length, 1);
  assert.equal(await publicClient.readContract({ ...counter, functionName: "count" }), 1n);

  // Event filter by ABI.
  const events = await publicClient.getContractEvents({
    ...counter,
    eventName: "Incremented",
    fromBlock: 0n,
    toBlock: "latest",
  });
  assert.equal(events.length, 1);
  assert.equal(events[0].args.by, account.address);
  assert.equal(events[0].args.newCount, 1n);

  // Custom error decodes from the code-3 revert data.
  await assert.rejects(
    publicClient.simulateContract({ ...counter, functionName: "incrementBy", args: [500n], account }),
    (error) => {
      const reverted = error.walk((e) => e instanceof ContractFunctionRevertedError);
      assert.ok(reverted, "viem decoded the revert data");
      assert.equal(reverted.data.errorName, "TooHigh");
      assert.deepEqual(reverted.data.args, [500n, 100n]);
      return true;
    },
  );
  // String revert reason surfaces in the message.
  await assert.rejects(
    publicClient.simulateContract({ ...counter, functionName: "fail", account }),
    (error) => {
      assert.match(error.message, /Counter: fail/);
      return true;
    },
  );

  // Snapshot, mutate, revert.
  const snapshot = await publicClient.request({ method: "evm_snapshot", params: [] });
  await publicClient.waitForTransactionReceipt({
    hash: await wallet.writeContract({ ...counter, functionName: "increment" }),
  });
  assert.equal(await publicClient.readContract({ ...counter, functionName: "count" }), 2n);
  assert.equal(await publicClient.request({ method: "evm_revert", params: [snapshot] }), true);
  assert.equal(await publicClient.readContract({ ...counter, functionName: "count" }), 1n);
  assert.equal(await publicClient.getBlockNumber({ cacheTime: 0 }), 2n);

  // Time travel is visible on the next block.
  const before = (await publicClient.getBlock()).timestamp;
  await publicClient.request({ method: "evm_increaseTime", params: [86_400] });
  await publicClient.request({ method: "evm_mine", params: [] });
  const after = (await publicClient.getBlock()).timestamp;
  assert.ok(after - before >= 86_400n, `${after} - ${before}`);
});

test("value transfer to a fresh address creates a funded hollow account", async (t) => {
  const { child, url } = await boot();
  t.after(() => child.kill());
  const account = privateKeyToAccount(KEY);
  const transport = http(url);
  const publicClient = createPublicClient({ chain, transport });
  const wallet = createWalletClient({ account, chain, transport });
  const to = "0x1234567890123456789012345678901234567890";
  const hash = await wallet.sendTransaction({ to, value: 5n * 10n ** 18n });
  const receipt = await publicClient.waitForTransactionReceipt({ hash });
  assert.equal(receipt.status, "success");
  assert.equal(receipt.gasUsed, 21_000n);
  assert.equal(await publicClient.getBalance({ address: to }), 5n * 10n ** 18n);
  // One weibar is not a tinybar: refused before execution, with the rule in the message.
  await assert.rejects(wallet.sendTransaction({ to, value: 1n }), /10\^10/);
});
