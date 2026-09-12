/**
 * Proves the rail works on this chain before the agent's app is judged against it.
 *
 * Starts the facilitator and the reference service, creates a receipt topic, makes one paid
 * request, and stops. The recipe's chain assertions then read the result: a topic with a
 * message on it, one settled CRYPTOTRANSFER, and no transaction refused before consensus.
 *
 * It deliberately does not test the generated app — the dev server does not exist yet at the
 * CHAIN stage. The app is judged by SMOKE and EVALUATE, in the browser.
 */

import { spawn } from "node:child_process";

const FACILITATOR_PORT = process.env.FACILITATOR_PORT ?? "4020";
const SERVICE_PORT = process.env.PORT ?? "4021";
const READY_TIMEOUT_MS = 60_000;

function run(command, env = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn("sh", ["-c", command], {
      stdio: ["ignore", "pipe", "inherit"],
      env: { ...process.env, ...env },
    });
    let out = "";
    child.stdout.on("data", (chunk) => {
      out += chunk;
      process.stdout.write(chunk);
    });
    child.on("exit", (code) =>
      code === 0 ? resolve(out) : reject(new Error(`${command} exited ${code}`)),
    );
  });
}

async function waitFor(url) {
  const deadline = Date.now() + READY_TIMEOUT_MS;
  while (Date.now() < deadline) {
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      // not listening yet
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error(`${url} did not answer within ${READY_TIMEOUT_MS} ms`);
}

const topicOutput = await run("npx tsx src/topic.ts");
const topicId = /TOLL_TOPIC_ID=(\S+)/.exec(topicOutput)?.[1];
if (topicId === undefined) {
  throw new Error("topic.ts did not print a topic id");
}

const rail = spawn("node", ["scripts/rail.mjs"], {
  stdio: "inherit",
  env: { ...process.env, TOLL_TOPIC_ID: topicId },
});

try {
  await waitFor(`http://127.0.0.1:${FACILITATOR_PORT}/health`);
  await waitFor(`http://127.0.0.1:${SERVICE_PORT}/`);
  await run("npx tsx src/pay.ts", { TOLL_TOPIC_ID: topicId });
  // The receipt is written after the response is on the wire; wait for it to reach the topic.
  const deadline = Date.now() + 30_000;
  for (;;) {
    const body = await (await fetch(`http://127.0.0.1:${SERVICE_PORT}/api/receipts`)).json();
    if (body.receipts?.length >= 1) {
      console.log(`[preflight] ${body.receipts.length} receipt(s) on ${topicId}`);
      break;
    }
    if (Date.now() > deadline) {
      throw new Error(`no receipt reached ${topicId} within 30 s`);
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
} finally {
  rail.kill("SIGTERM");
}
