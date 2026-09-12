/**
 * The metered service. One free route, one x402-gated route, a usage meter, and the receipts.
 *
 * The gate is x402's own Express middleware. What this file adds is the part the protocol leaves
 * to the server: every settlement is written to an HCS topic as an `x402.receipt.v1` message, so
 * the payment history is provable from the chain rather than from these logs.
 */

import express, { type Request, type Response } from "express";
import { HTTPFacilitatorClient } from "@x402/core/server";
import { paymentMiddleware, x402ResourceServer } from "@x402/express";
import { ExactHederaScheme } from "@x402/hedera/exact/server";
import {
  CAIP2,
  FACILITATOR_URL,
  HBAR_ASSET,
  PRICE_TINYBARS,
  SERVICE_PORT,
  TOLL_NETWORK,
  TOPIC_ID,
  payTo,
} from "./config.js";
import { type Receipt, receipts, writeReceipt } from "./hedera.js";

const PAID_ROUTE = "/api/data/paid";
const destination = payTo();

/** Settlement result, base64 JSON in the PAYMENT-RESPONSE header (transports-v2/http.md:113). */
interface SettlementResponse {
  success: boolean;
  transaction: string;
  network: string;
  payer: string;
  errorReason?: string;
}

const meter = { calls: 0, tinybars: 0n, settlements: 0, refusals: 0 };

const app = express();

app.use(
  paymentMiddleware(
    {
      [`GET ${PAID_ROUTE}`]: {
        accepts: [
          {
            scheme: "exact",
            // HBAR is quoted in tinybar, not dollars: @x402/hedera README, "Amount Units".
            price: { amount: PRICE_TINYBARS, asset: HBAR_ASSET },
            network: CAIP2,
            payTo: destination,
          },
        ],
        description: "One metered reading, paid per call",
        mimeType: "application/json",
      },
    },
    new x402ResourceServer(new HTTPFacilitatorClient({ url: FACILITATOR_URL })).register(
      CAIP2,
      new ExactHederaScheme(),
    ),
  ),
);

/** Reads the settlement off the response headers once the reply is on the wire. */
function onSettled(request: Request, response: Response): void {
  response.on("finish", () => {
    const header = response.getHeader("PAYMENT-RESPONSE");
    if (typeof header !== "string") {
      return;
    }
    let settled: SettlementResponse;
    try {
      settled = JSON.parse(Buffer.from(header, "base64").toString("utf8")) as SettlementResponse;
    } catch {
      console.warn("[toll] PAYMENT-RESPONSE was not base64 JSON; not metered");
      return;
    }
    if (!settled.success) {
      meter.refusals += 1;
      console.log(`[toll] refused ${settled.errorReason ?? "no reason given"}`);
      return;
    }
    meter.settlements += 1;
    meter.tinybars += BigInt(PRICE_TINYBARS);
    console.log(`[toll] settled ${settled.transaction} payer ${settled.payer}`);
    void recordReceipt(request.path, settled);
  });
}

async function recordReceipt(route: string, settled: SettlementResponse): Promise<void> {
  if (TOPIC_ID === undefined) {
    return;
  }
  const receipt: Receipt = {
    type: "x402.receipt.v1",
    network: settled.network,
    chain: TOLL_NETWORK,
    route,
    payer: settled.payer,
    payTo: destination,
    asset: HBAR_ASSET,
    amount: PRICE_TINYBARS,
    transaction: settled.transaction,
    at: new Date().toISOString(),
  };
  try {
    await writeReceipt(TOPIC_ID, receipt);
  } catch (error) {
    // The payment settled; the audit entry did not. Say which, and do not fail the caller's
    // request after the fact.
    console.error(
      `[toll] receipt for ${settled.transaction} not written to ${TOPIC_ID}: ` +
        `${error instanceof Error ? error.message : "unknown error"}`,
    );
  }
}

// The root describes the service. `hanvil toll` and the smoke gate both wait for a 200 here.
app.get("/", (_request, response) => {
  response.json({
    service: "toll",
    network: CAIP2,
    chain: TOLL_NETWORK,
    facilitator: FACILITATOR_URL,
    topicId: TOPIC_ID ?? null,
    routes: {
      free: "/api/data/free",
      paid: `${PAID_ROUTE} (${PRICE_TINYBARS} tinybar -> ${destination})`,
      usage: "/api/usage",
      receipts: "/api/receipts",
    },
  });
});

app.get("/api/data/free", (_request, response) => {
  meter.calls += 1;
  response.json({ reading: 21.4, unit: "celsius", paid: false, at: new Date().toISOString() });
});

app.get(PAID_ROUTE, (request, response) => {
  meter.calls += 1;
  onSettled(request, response);
  response.json({
    reading: 21.4,
    unit: "celsius",
    paid: true,
    priceTinybars: PRICE_TINYBARS,
    at: new Date().toISOString(),
  });
});

app.get("/api/usage", (_request, response) => {
  response.json({
    network: CAIP2,
    facilitator: FACILITATOR_URL,
    priceTinybars: PRICE_TINYBARS,
    payTo: destination,
    topicId: TOPIC_ID ?? null,
    calls: meter.calls,
    settlements: meter.settlements,
    refusals: meter.refusals,
    // In memory, so it resets when this process does. The topic is the durable record.
    meteredTinybars: meter.tinybars.toString(),
  });
});

app.get("/api/receipts", async (_request, response) => {
  if (TOPIC_ID === undefined) {
    response
      .status(503)
      .json({ error: "TOLL_TOPIC_ID is not set; no receipt topic exists. Run `yarn topic`." });
    return;
  }
  try {
    response.json({ topicId: TOPIC_ID, receipts: await receipts(TOPIC_ID) });
  } catch (error) {
    response
      .status(502)
      .json({ error: error instanceof Error ? error.message : "unknown error" });
  }
});

app.get("/health", (_request, response) => {
  response.json({ status: "ok", network: TOLL_NETWORK, caip2: CAIP2 });
});

app.listen(SERVICE_PORT, () => {
  console.log(`[toll] Local: http://127.0.0.1:${SERVICE_PORT}`);
  console.log(`[toll] network      ${CAIP2} (${TOLL_NETWORK})`);
  console.log(`[toll] facilitator  ${FACILITATOR_URL}`);
  console.log(`[toll] ${PAID_ROUTE}  ${PRICE_TINYBARS} tinybar -> ${destination}`);
});
