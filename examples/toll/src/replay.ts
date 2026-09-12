/**
 * What a replayed payment looks like, and where it can be read.
 *
 * A PAYMENT-SIGNATURE header is a signed transfer. Replaying one is the obvious attack on a
 * metered API, and the protocol's answer is the chain's: the transaction id has already reached
 * consensus, so the node refuses the second copy with DUPLICATE_TRANSACTION — before consensus,
 * which on Hedera means no record, no receipt and no mirror row is written for it. The facilitator
 * reports a failed settlement and the service answers 402. Nothing anywhere says why.
 *
 * hanvil is the node, so it keeps the refusal. This script pays once, replays the header, and
 * then reads the refusal back off `hanvil_rejections` — the thing no mirror node can answer.
 */

import { x402Client, wrapFetchWithPayment } from "@x402/fetch";
import { createClientHederaSigner, PrivateKey } from "@x402/hedera";
import { ExactHederaScheme } from "@x402/hedera/exact/client";
import { CAIP2, HBAR_ASSET, TOLL_NETWORK, nodeUrl, payer } from "./config.js";

const BASE_URL = process.env.TOLL_URL ?? "http://127.0.0.1:4021";
const ROUTE = "/api/data/paid";
const RPC_URL = process.env.HANVIL_RPC_URL ?? "http://127.0.0.1:7546";

/** Wraps fetch so the payment header the client produced can be replayed verbatim. */
function recordingFetch(sink: { header?: string }): typeof fetch {
  return async (input, init) => {
    // The header may arrive either in `init` or already on a Request built by the caller.
    const fromInit = new Headers(init?.headers).get("PAYMENT-SIGNATURE");
    const fromRequest =
      input instanceof Request ? input.headers.get("PAYMENT-SIGNATURE") : null;
    const signature = fromInit ?? fromRequest;
    if (signature !== null) {
      sink.header = signature;
    }
    return fetch(input, init);
  };
}

async function rejections(): Promise<unknown[]> {
  const response = await fetch(RPC_URL, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "hanvil_rejections", params: [] }),
  });
  const body = (await response.json()) as { result?: unknown[] };
  return body.result ?? [];
}

async function main(): Promise<void> {
  if (TOLL_NETWORK !== "local") {
    throw new Error(
      "This script reads hanvil_rejections, which only the local chain answers. On testnet a " +
        "refused settlement leaves nothing to read, which is the point it is making.",
    );
  }

  const { accountId, privateKey } = payer();
  const signer = createClientHederaSigner(accountId, PrivateKey.fromStringECDSA(privateKey), {
    network: CAIP2,
    nodeUrl: nodeUrl(),
  });
  const client = new x402Client();
  client.setSpendControls({
    allowedAssets: [{ network: CAIP2, asset: HBAR_ASSET, maxAmountPerPayment: "1000000" }],
  });
  client.register("hedera:*", new ExactHederaScheme(signer));

  const before = (await rejections()).length;
  const captured: { header?: string } = {};
  const url = `${BASE_URL}${ROUTE}`;

  const paid = await wrapFetchWithPayment(recordingFetch(captured), client)(url, { method: "GET" });
  console.log(`[replay] first request  ${paid.status}  ${settlement(paid)}`);
  if (captured.header === undefined) {
    throw new Error("no PAYMENT-SIGNATURE header was produced; nothing to replay");
  }

  const replayed = await fetch(url, {
    method: "GET",
    headers: { "PAYMENT-SIGNATURE": captured.header },
  });
  console.log(`[replay] replayed once ${replayed.status}  ${settlement(replayed)}`);

  const after = await rejections();
  console.log(`\n[replay] hanvil_rejections grew by ${after.length - before}:`);
  for (const entry of after.slice(before)) {
    console.dir(entry, { depth: null });
  }
  console.log(
    "\n[replay] On testnet that refusal has no transaction, no receipt and no mirror row.\n" +
      "[replay] The 402 above is the only trace a developer would ever see.",
  );
}

function settlement(response: Response): string {
  const header = response.headers.get("PAYMENT-RESPONSE");
  if (header === null) {
    return "no PAYMENT-RESPONSE";
  }
  const settled = JSON.parse(Buffer.from(header, "base64").toString("utf8")) as {
    success: boolean;
    transaction: string;
    errorReason?: string;
  };
  return settled.success
    ? `settled ${settled.transaction}`
    : `refused ${settled.errorReason ?? "no reason given"}`;
}

main().catch((error: unknown) => {
  console.error(error instanceof Error ? error.message : error);
  process.exit(1);
});
