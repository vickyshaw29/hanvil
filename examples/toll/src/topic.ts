/** Creates the receipt topic on the configured network and prints the id to export. */

import { createTopic } from "./hedera.js";
import { TOLL_NETWORK } from "./config.js";

const topicId = await createTopic();
console.log(`[toll] topic ${topicId} created on ${TOLL_NETWORK}`);
console.log(`export TOLL_TOPIC_ID=${topicId}`);
if (TOLL_NETWORK === "testnet") {
  console.log(`https://hashscan.io/testnet/topic/${topicId}`);
}
