/**
 * Starts the local rail in the order it has to start in.
 *
 * `x402ResourceServer` fetches the facilitator's /supported before it can quote a price, so a
 * service that takes its first request while the facilitator is still booting answers 500. This
 * waits for /health, then starts the service, and takes both down together.
 *
 * `hanvil toll` does the same thing in Rust. This is what it replaces.
 */

import { spawn } from "node:child_process";

const FACILITATOR_PORT = process.env.FACILITATOR_PORT ?? "4020";
const READY_URL = `http://127.0.0.1:${FACILITATOR_PORT}/health`;
const READY_TIMEOUT_MS = 30_000;

const children = [];

function start(name, args) {
  const child = spawn("npx", ["tsx", ...args], { stdio: "inherit" });
  child.on("exit", (code, signal) => {
    if (signal === "SIGTERM" || signal === "SIGINT") return;
    console.error(`[rail] ${name} exited with ${code ?? signal}; stopping the rail`);
    stop(code ?? 1);
  });
  children.push(child);
  return child;
}

function startShell(name, command) {
  const child = spawn("sh", ["-c", command], { stdio: "inherit" });
  child.on("exit", (code, signal) => {
    if (signal === "SIGTERM" || signal === "SIGINT") return;
    console.error(`[rail] ${name} exited with ${code ?? signal}; stopping the rail`);
    stop(code ?? 1);
  });
  children.push(child);
  return child;
}

function stop(code) {
  for (const child of children) child.kill("SIGTERM");
  process.exit(code);
}

async function ready() {
  const deadline = Date.now() + READY_TIMEOUT_MS;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(READY_URL);
      if (response.ok) return;
    } catch {
      // not listening yet
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error(`facilitator did not answer ${READY_URL} within ${READY_TIMEOUT_MS} ms`);
}

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => stop(0));
}

/**
 * What the rail serves in front of the facilitator. Defaults to the reference service in
 * `src/service.ts`; the generated app sets it to its own dev server.
 */
const SERVICE_COMMAND = process.env.RAIL_SERVICE_COMMAND;

start("facilitator", ["src/facilitator.ts"]);
await ready();
if (SERVICE_COMMAND === undefined) {
  start("service", ["src/service.ts"]);
} else {
  startShell("service", SERVICE_COMMAND);
}
