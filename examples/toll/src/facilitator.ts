/**
 * The local x402 facilitator: /supported, /verify, /settle, /health.
 *
 * This is x402's own reference facilitator, not a reimplementation of one. The only thing
 * changed is `buildHederaClient`, which points it at the hanvil chain in this workspace instead
 * of testnet — the extension point the reference example itself uses
 * (x402-foundation/x402, examples/typescript/facilitator/advanced/all_networks.ts:309-323).
 *
 * Production does not run this. On testnet the facilitator is Blocky402, and FACILITATOR_URL
 * says so. This exists so the loop that develops an x402 service costs no HBAR and no network:
 * a settlement refused before consensus leaves no record on any Hedera mirror, and hanvil is the
 * node, so it is the only place that refusal can be read back.
 */

import express from "express";
import { x402Facilitator } from "@x402/core/facilitator";
import type { PaymentPayload, PaymentRequirements } from "@x402/core/types";
import { ExactHederaScheme } from "@x402/hedera/exact/facilitator";
import {
  AccountId,
  Client,
  PrivateKey,
  createHederaPreflightTransfer,
  createHederaSignAndSubmitTransaction,
  createHederaVerifyPayerSignature,
  toFacilitatorHederaSigner,
} from "@x402/hedera";
import { CAIP2, FACILITATOR_PORT, MIRROR_URL, TOLL_NETWORK, localGrpcUrl } from "./config.js";

const LOCAL_NODE_ACCOUNT = "0.0.3";

if (TOLL_NETWORK === "testnet") {
  throw new Error(
    "The bundled facilitator settles on the local hanvil chain and HEDERA_NETWORK is " +
      '"testnet". On testnet the facilitator is Blocky402 — leave FACILITATOR_URL at its ' +
      "default and do not start this process.",
  );
}

function requiredEnv(name: string): string {
  const value = process.env[name];
  if (value === undefined || value === "") {
    throw new Error(
      `${name} is not set. The facilitator co-signs and submits the transfer, so it needs an ` +
        "account of its own on the local chain. hanvil's boot banner lists thirty funded " +
        "accounts with their keys; any of them will do, as long as it is not the payer.",
    );
  }
  return value;
}

const accountId = requiredEnv("FACILITATOR_ACCOUNT_ID");
const key = PrivateKey.fromStringECDSA(requiredEnv("FACILITATOR_PRIVATE_KEY"));

/** A client on the chain this workspace owns. The whole local rail is this one function. */
const buildHederaClient = (): Client =>
  Client.forNetwork({
    [localGrpcUrl()]: AccountId.fromString(LOCAL_NODE_ACCOUNT),
  }).setOperator(AccountId.fromString(accountId), key);

const signer = toFacilitatorHederaSigner({
  getAddresses: () => [accountId],
  signAndSubmitTransaction: createHederaSignAndSubmitTransaction(buildHederaClient, key),
  verifyPayerSignature: createHederaVerifyPayerSignature({ mirrorNodeUrl: MIRROR_URL }),
  // Both of these resolve an account off a mirror node, and without mirrorNodeUrl they
  // derive it from the CAIP-2 id — the public testnet mirror, asked about a hanvil account
  // it has never heard of. The verifier reads the payer's key from there, so the wrong
  // mirror reports a wrong key and every partial signature is refused as invalid.
  // hanvil serves the same REST shape, so pointing both at it is the whole fix.
  preflightTransfer: createHederaPreflightTransfer({ mirrorNodeUrl: MIRROR_URL }),
});

const facilitator = new x402Facilitator().register(CAIP2, new ExactHederaScheme(signer));

const app = express();
app.use(express.json());

app.post("/verify", async (request, response) => {
  const { paymentPayload, paymentRequirements } = request.body as {
    paymentPayload?: PaymentPayload;
    paymentRequirements?: PaymentRequirements;
  };
  if (!paymentPayload || !paymentRequirements) {
    response.status(400).json({ error: "Missing paymentPayload or paymentRequirements" });
    return;
  }
  try {
    response.json(await facilitator.verify(paymentPayload, paymentRequirements));
  } catch (error) {
    response.status(500).json({ error: message(error) });
  }
});

app.post("/settle", async (request, response) => {
  const { paymentPayload, paymentRequirements } = request.body as {
    paymentPayload?: PaymentPayload;
    paymentRequirements?: PaymentRequirements;
  };
  if (!paymentPayload || !paymentRequirements) {
    response.status(400).json({ error: "Missing paymentPayload or paymentRequirements" });
    return;
  }
  try {
    const settled = await facilitator.settle(paymentPayload, paymentRequirements);
    console.log(
      `[toll] settle ${settled.success ? "SUCCESS" : "REFUSED"} ` +
        `${settled.transaction || (settled.errorReason ?? "")}`,
    );
    response.json(settled);
  } catch (error) {
    // A refusal before consensus arrives here, and it is the interesting case: no record, no
    // receipt and no mirror row exists for it. `hanvil_rejections` is where it can be read.
    console.log(`[toll] settle THREW ${message(error)}`);
    response.status(500).json({ error: message(error) });
  }
});

app.get("/supported", (_request, response) => {
  response.json(facilitator.getSupported());
});

app.get("/health", (_request, response) => {
  response.json({ status: "ok", network: CAIP2, feePayer: accountId });
});

function message(error: unknown): string {
  return error instanceof Error ? error.message : "unknown error";
}

app.listen(FACILITATOR_PORT, "127.0.0.1", () => {
  console.log(`[toll] x402 facilitator  127.0.0.1:${FACILITATOR_PORT}`);
  console.log(`[toll] settles on        the local hanvil chain (${localGrpcUrl()})`);
  console.log(`[toll] feePayer          ${accountId}`);
  console.log(`[toll] network           ${CAIP2}`);
});
