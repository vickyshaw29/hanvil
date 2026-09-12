/**
 * Configuration, read once from the environment.
 *
 * Nothing is defaulted silently: a missing value throws and names what was expected and why.
 * The one switch that matters is HEDERA_NETWORK, which picks the rail — the in-process hanvil
 * chain through the bundled reference facilitator, or Hedera testnet through Blocky402.
 */

import type { Network } from "@x402/core/types";

export type TollNetwork = "local" | "testnet";

/** Blocky402's testnet facilitator. Read from its /supported on 2026-09-12. */
export const BLOCKY402_TESTNET = "https://api.testnet.blocky402.com";

/** HBAR, as the x402 hedera scheme names it. Amounts against it are in tinybar. */
export const HBAR_ASSET = "0.0.0";

function required(name: string, why: string): string {
  const value = process.env[name];
  if (value === undefined || value === "") {
    throw new Error(`${name} is not set. ${why}`);
  }
  return value;
}

function network(): TollNetwork {
  const value = process.env.HEDERA_NETWORK ?? "local";
  if (value !== "local" && value !== "testnet") {
    throw new Error(
      `HEDERA_NETWORK is "${value}"; expected "local" (settle on hanvil) or "testnet" ` +
        `(settle on Hedera testnet through Blocky402). Mainnet is not supported.`,
    );
  }
  return value;
}

export const TOLL_NETWORK = network();

/**
 * The CAIP-2 id every quote is written in.
 *
 * `hedera:testnet` even on the local rail. `@x402/hedera` hardcodes
 * `SUPPORTED_HEDERA_NETWORKS = ["hedera:mainnet", "hedera:testnet"]` and asserts against it in
 * the server scheme, the client signer and the facilitator, so a local node cannot be quoted
 * under an id of its own: `hedera:localnet` is refused with `Unsupported Hedera network`. The id
 * names the payment rail, and `nodeUrl()` names the chain — on the local rail those disagree on
 * purpose, and nothing reaches testnet because every component is pointed at hanvil.
 */
export const CAIP2 = "hedera:testnet" as Network;

/**
 * The HAPI endpoint every component talks to, `host:port`, or undefined for the SDK's own
 * network. `createHederaClient` takes this as an override and builds
 * `Client.forNetwork({ [nodeUrl]: 0.0.3 })` from it, which is exactly hanvil's shape.
 */
export function nodeUrl(): string | undefined {
  return TOLL_NETWORK === "local" ? localGrpcUrl() : process.env.TOLL_NODE_URL;
}

/**
 * Where /verify and /settle are answered. Local runs point at the bundled reference
 * facilitator; testnet points at Blocky402, which is the one the prize requires.
 */
export const FACILITATOR_URL =
  process.env.FACILITATOR_URL ??
  (TOLL_NETWORK === "testnet"
    ? BLOCKY402_TESTNET
    : `http://127.0.0.1:${process.env.FACILITATOR_PORT ?? "4020"}`);

/** Mirror node REST base. hanvil serves its own; testnet has the public one. */
export const MIRROR_URL =
  TOLL_NETWORK === "testnet"
    ? (process.env.MIRROR_URL ?? "https://testnet.mirrornode.hedera.com")
    : required(
        "HANVIL_MIRROR_URL",
        'On network "local" the mirror is hanvil\'s own. Start `hanvil` and it is exported ' +
          "for every child process, or set it to http://127.0.0.1:5551.",
      );

/** HAPI gRPC endpoint of the local node, `host:port`. Only meaningful on "local". */
export function localGrpcUrl(): string {
  return required(
    "HANVIL_GRPC_URL",
    'On network "local" the chain is hanvil. Start `hanvil` and it is exported for every ' +
      "child process, or set it to 127.0.0.1:50211.",
  );
}

/** The account that pays for calls, and signs the transfer the facilitator submits. */
export function payer(): { accountId: string; privateKey: string } {
  return {
    accountId: required(
      "HEDERA_ACCOUNT_ID",
      "The paying account, as 0.0.N. On testnet, create one at portal.hedera.com.",
    ),
    privateKey: required(
      "HEDERA_PRIVATE_KEY",
      "The paying account's ECDSA key, 0x-prefixed. ED25519 has no EVM alias and the " +
        "x402 hedera scheme's live path assumes ECDSA.",
    ),
  };
}

/** Where a settled toll lands. Must differ from the payer: paying yourself nets to zero. */
export function payTo(): string {
  const destination = required(
    "PAY_TO",
    "The account a settled toll is paid to, as 0.0.N. It must not be HEDERA_ACCOUNT_ID.",
  );
  if (destination === process.env.HEDERA_ACCOUNT_ID) {
    throw new Error(
      `PAY_TO is ${destination}, the same account as HEDERA_ACCOUNT_ID. A transfer to ` +
        "itself nets to zero and the facilitator's preflight refuses it. Use a second account.",
    );
  }
  return destination;
}

/** The price of one call to the gated route, in tinybar. 100000 tinybar is 0.001 ℏ. */
export const PRICE_TINYBARS = process.env.TOLL_PRICE_TINYBARS ?? "100000";

/** The HCS topic receipts are written to. Unset means receipts are not written. */
export const TOPIC_ID = process.env.TOLL_TOPIC_ID;

export const SERVICE_PORT = Number(process.env.PORT ?? "4021");
export const FACILITATOR_PORT = Number(process.env.FACILITATOR_PORT ?? "4020");
