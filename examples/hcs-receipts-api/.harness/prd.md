# Receipts API on HCS

## Goal

A small HTTP JSON API in Node 22 (no framework) that logs payment receipts to a Hedera Consensus
Service topic on the local Hedera network this harness runs, and reads them back from the mirror
node. "Working" means: `node scripts/seed.js` creates the topic and posts the first receipt,
`node server.js` serves three routes against that topic, and `npm test` passes.

## Environment

The harness runs a local Hedera network and tells you where it is. Read these from `process.env`;
never hard-code endpoints or keys:

- `HANVIL_GRPC_URL` — HAPI gRPC, `host:port`. Build the SDK client with
  `Client.forNetwork({ [HANVIL_GRPC_URL]: AccountId.fromString("0.0.3") })`.
- `HANVIL_MIRROR_URL` — mirror node REST base URL. Read topic messages with
  `fetch(\`${HANVIL_MIRROR_URL}/api/v1/topics/${topicId}/messages\`)`; the mirror's gRPC
  subscription is not available, so do not use `TopicMessageQuery`.
- `HARNESS_SIGNER_ACCOUNT_ID` and `HARNESS_SIGNER_PRIVATE_KEY` — a funded ECDSA operator
  (`PrivateKey.fromStringECDSA`). Use it to pay for the topic and every message.

`@hiero-ledger/sdk` is already in `package.json`; run `npm install` before anything else.

## Deliverables

1. `scripts/seed.js` — creates a topic (`TopicCreateTransaction`), posts one welcome receipt
   (`TopicMessageSubmitTransaction` with JSON `{ "kind": "welcome", "at": <ISO time> }`), and
   writes `topic.json` at the project root: `{ "topicId": "0.0.N" }`. Idempotent: if `topic.json`
   exists and the topic answers on the mirror, reuse it and post nothing new.
2. `server.js` — reads `topic.json` and listens on `127.0.0.1:3000`. Print exactly
   `Local: http://127.0.0.1:3000` once it is listening.
   - `GET /` → `200` JSON `{ "service": "receipts", "topicId": "0.0.N", "network": "local" }`.
   - `POST /receipts` with JSON `{ "amount": number, "memo": string }` → submits the receipt as a
     JSON message to the topic and answers `201` with `{ "sequenceNumber", "consensusTimestamp" }`
     from the receipt/record. Bad input answers `400` with `{ "error" }`.
   - `GET /receipts` → `200` JSON `{ "topicId", "receipts": [...] }` read from the mirror, each
     message base64-decoded and parsed, oldest first.
   - Every other path → `404` JSON `{ "error": "not found" }`.
   - Never `Application error` in a response body.
3. `test/server.test.js` — `node --test`: starts the server on a random port (pass `PORT`),
   checks `GET /` shape and that `POST /receipts` then `GET /receipts` shows the new receipt.
   The tests need the network too; skip with a message when `HANVIL_GRPC_URL` is unset.
4. `README.md` — how to seed, run, test; the three routes; that everything is local.

## Non-goals

No frontend, no wallet, no HTS tokens, no Express, no TypeScript, no Docker, no testnet.
