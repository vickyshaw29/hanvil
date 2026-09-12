/**
 * The paying agent.
 *
 * It asks for a reading, is told 402 with the price, decides the price is within its budget,
 * signs the transfer, and asks again. The decision is `setSpendControls` — the agent will not
 * pay more than it was told it may, and a service that asks for more gets nothing.
 */

import { x402Client, wrapFetchWithPayment, x402HTTPClient } from "@x402/fetch";
import { createClientHederaSigner, PrivateKey } from "@x402/hedera";
import { ExactHederaScheme } from "@x402/hedera/exact/client";
import { CAIP2, HBAR_ASSET, TOLL_NETWORK, nodeUrl, payer } from "./config.js";

const BASE_URL = process.env.TOLL_URL ?? "http://127.0.0.1:4021";
const ROUTE = process.env.TOLL_ROUTE ?? "/api/data/paid";
/** The most this agent will pay for one call, in tinybar. The service asks for 100000. */
const MAX_TINYBARS = process.env.TOLL_MAX_TINYBARS ?? "1000000";

async function main(): Promise<void> {
  const { accountId, privateKey } = payer();
  // nodeUrl points the signer at hanvil on the local rail and is undefined on testnet,
  // where the SDK's own network is what we want.
  const signer = createClientHederaSigner(accountId, PrivateKey.fromStringECDSA(privateKey), {
    network: CAIP2,
    nodeUrl: nodeUrl(),
  });

  const client = new x402Client();
  // HBAR is not one of the default assets spend controls recognise — those are USDC and the
  // like, capped in dollars — so it is opted into by id with an atomic cap in tinybar. This is
  // the agent's decision: a service that quotes more than this gets nothing.
  client.setSpendControls({
    allowedAssets: [{ network: CAIP2, asset: HBAR_ASSET, maxAmountPerPayment: MAX_TINYBARS }],
  });
  client.register("hedera:*", new ExactHederaScheme(signer));

  const url = `${BASE_URL}${ROUTE}`;
  console.log(`[agent] ${accountId} asking ${url}, at most ${MAX_TINYBARS} tinybar per call`);

  const unpaid = await fetch(url);
  console.log(`[agent] unpaid request answered ${unpaid.status}`);

  const started = performance.now();
  const paid = await wrapFetchWithPayment(fetch, client)(url, { method: "GET" });
  const body = await new x402HTTPClient(client).processResponse(paid);
  const elapsed = ((performance.now() - started) / 1000).toFixed(3);

  console.log(`[agent] paid request answered ${paid.status} in ${elapsed}s`);
  console.dir(body, { depth: null });

  const settlement = paid.headers.get("PAYMENT-RESPONSE");
  if (settlement !== null) {
    const settled = JSON.parse(Buffer.from(settlement, "base64").toString("utf8")) as {
      success: boolean;
      transaction: string;
      network: string;
    };
    console.log(`[agent] settlement ${settled.success ? "SUCCESS" : "REFUSED"} ${settled.transaction}`);
    // On the CAIP-2 id alone this is indistinguishable from testnet, and on the local rail
    // it would print a HashScan link for a transaction HashScan has never seen.
    if (settled.success && TOLL_NETWORK === "testnet") {
      console.log(`[agent] https://hashscan.io/testnet/transaction/${settled.transaction}`);
    }
  }
}

main().catch((error: unknown) => {
  console.error(error instanceof Error ? error.message : error);
  process.exit(1);
});
