/**
 * The Hedera client, the receipt topic, and the mirror read.
 *
 * `@hiero-ledger/sdk` is pinned to 2.85.0, the version `@x402/hedera` itself depends on, so the
 * two resolve to one on-disk copy. Two copies break the SDK's string-brand checks with
 * `t.startsWith is not a function` — see @x402/hedera's README, "Hedera SDK primitives".
 * Primitives both halves share are imported from `@x402/hedera` for the same reason; only the
 * consensus-service transactions, which it does not re-export, come from the SDK directly.
 */

import { AccountId, Client, PrivateKey } from "@x402/hedera";
import { TopicCreateTransaction, TopicMessageSubmitTransaction } from "@hiero-ledger/sdk";
import { CAIP2, MIRROR_URL, TOLL_NETWORK, localGrpcUrl, payer } from "./config.js";

/** The node account every hanvil HAPI request is addressed to, as hiero-local-node uses. */
const LOCAL_NODE_ACCOUNT = "0.0.3";

/**
 * A client for whichever network this process is configured for.
 *
 * On "local" this is the in-process hanvil chain, reached over plaintext gRPC. No mirror network
 * is set: hanvil does not serve the mirror's gRPC API on 5600, so `TopicMessageQuery` would
 * retry twenty times and give up. Topic messages are read over REST instead, in `receipts()`.
 */
export function client(): Client {
  const { accountId, privateKey } = payer();
  const operator = AccountId.fromString(accountId);
  const key = PrivateKey.fromStringECDSA(privateKey);
  const configured =
    TOLL_NETWORK === "testnet"
      ? Client.forTestnet()
      : Client.forNetwork({ [localGrpcUrl()]: AccountId.fromString(LOCAL_NODE_ACCOUNT) });
  return configured.setOperator(operator, key);
}

/** Creates the receipt topic and answers its id. Run once; pass the id back as TOLL_TOPIC_ID. */
export async function createTopic(): Promise<string> {
  const connection = client();
  try {
    const submitted = await new TopicCreateTransaction()
      .setTopicMemo(`toll x402 receipts (${CAIP2})`)
      .execute(connection);
    const receipt = await submitted.getReceipt(connection);
    const topicId = receipt.topicId;
    if (topicId === null) {
      throw new Error("TopicCreateTransaction succeeded without a topic id in its receipt.");
    }
    return topicId.toString();
  } finally {
    connection.close();
  }
}

/** The receipt envelope the PRD specifies: `x402.receipt.v1`. */
export interface Receipt {
  type: "x402.receipt.v1";
  /** The CAIP-2 id the payment was quoted in, as the settlement reported it. */
  network: string;
  /**
   * Which chain it actually settled on. `network` alone cannot say: the local rail is quoted as
   * `hedera:testnet` because `@x402/hedera` admits no other id, so a receipt without this field
   * would claim testnet for a payment that never left this machine.
   */
  chain: "local" | "testnet";
  route: string;
  payer: string;
  payTo: string;
  asset: string;
  amount: string;
  transaction: string;
  at: string;
}

/**
 * Writes one receipt to the topic. The audit trail is the point: a settlement that happened is
 * provable from the topic alone, without trusting this service's own logs.
 */
export async function writeReceipt(topicId: string, receipt: Receipt): Promise<void> {
  const connection = client();
  try {
    const submitted = await new TopicMessageSubmitTransaction()
      .setTopicId(topicId)
      .setMessage(JSON.stringify(receipt))
      .execute(connection);
    await submitted.getReceipt(connection);
  } finally {
    connection.close();
  }
}

/**
 * Reads the receipts back off the mirror node, oldest first.
 *
 * The same REST shape serves hanvil and testnet — `/api/v1/topics/{id}/messages` with
 * base64 `message` — which is the reason the service does not care which network it is on.
 */
export async function receipts(topicId: string, limit = 25): Promise<unknown[]> {
  const url = `${MIRROR_URL}/api/v1/topics/${topicId}/messages?order=asc&limit=${limit}`;
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`mirror ${response.status} for ${url}`);
  }
  const body = (await response.json()) as { messages?: { message: string }[] };
  return (body.messages ?? []).map((entry) => {
    const decoded = Buffer.from(entry.message, "base64").toString("utf8");
    try {
      return JSON.parse(decoded) as unknown;
    } catch {
      return decoded;
    }
  });
}
