# Toll — x402 payments on the local chain

A metered API gated by [x402](https://x402.org), settled in HBAR. On `hanvil` it settles in
0.112 s against a chain that boots in 2 ms and costs nothing. On Hedera testnet the same service
settles through the [Blocky402](https://blocky402.com) facilitator. One environment variable
picks the rail.

An x402 payment on Hedera is a `CryptoTransfer` the client partially signs and the facilitator
co-signs and submits as fee payer. This is one, read off `hanvil`'s mirror after a single paid
request:

```
0.0.1004-1789194758-592048710 CRYPTOTRANSFER SUCCESS
  0.0.98    +10000   the fee
  0.0.1002 -100000   the payer
  0.0.1003 +100000   payTo
  0.0.1004  -10000   the facilitator, which paid the fee
```

## Why it is here

A settlement the node refuses is refused **before consensus**, and on Hedera that means no
record, no receipt and no mirror row is written anywhere. The error returned to the caller is the
only trace, and x402's middleware folds it into a 402. Replay a payment header and this is what
the two sides see:

```
$ yarn replay
[toll] first request  200  settled 0.0.1004@1789194847.284997026
[toll] replayed once  402  refused transaction_failed          <- all the protocol will tell you

[toll] hanvil_rejections grew by 1:
{ kind: 'CRYPTOTRANSFER', payer: '0.0.1004', code: 11, reason: 'DUPLICATE_TRANSACTION' }
```

`transaction_failed` is what a developer on testnet gets, and there is nothing on any mirror node
to look up. `hanvil` is the node, so it keeps the refusal: `hanvil_rejections` over JSON-RPC, the
chain ledger during a `hanvil run`, and `rejections` chain assertions in a recipe. After the three
paid requests above, `hanvil`'s mirror held three `CRYPTOTRANSFER SUCCESS` rows and nothing else;
the duplicate existed only in that list.

## Run it

Two terminals, no credentials, no testnet account.

```
hanvil                                   # note two account ids and keys from the banner
cd examples/toll && yarn install

export HANVIL_GRPC_URL=127.0.0.1:50211 HANVIL_MIRROR_URL=http://127.0.0.1:5551
export HEDERA_ACCOUNT_ID=0.0.1002 HEDERA_PRIVATE_KEY=0x7f109a9e…   # the payer
export PAY_TO=0.0.1003                                             # a different account
export FACILITATOR_ACCOUNT_ID=0.0.1004 FACILITATOR_PRIVATE_KEY=0xb4d7f7e8…

yarn topic && export TOLL_TOPIC_ID=0.0.1032   # the receipt topic it prints
yarn rail                                     # facilitator on :4020, service on :4021
yarn pay                                      # the paying agent
yarn replay                                   # the refusal no mirror node has
```

`HEDERA_NETWORK` defaults to `local`. The keys above are `hanvil`'s predefined accounts — public,
deterministic, and worthless. Nothing here is a secret and nothing here is written to a file.

## What is on the wire

| | |
| --- | --- |
| `GET /api/data/free` | a reading, no payment |
| `GET /api/data/paid` | 402 with `PAYMENT-REQUIRED`, then the reading once paid |
| `GET /api/usage` | calls, settlements, refusals, tinybars metered — in memory |
| `GET /api/receipts` | the receipts, read back off the mirror node |
| `GET /health` | the network and the CAIP-2 id it quotes in |

Headers are x402 v2's: `PAYMENT-REQUIRED`, `PAYMENT-SIGNATURE`, `PAYMENT-RESPONSE`
(`specs/transports-v2/http.md`), not v1's `X-PAYMENT`. The 402 carries the price in tinybar and
the facilitator's fee payer:

```json
{ "scheme": "exact", "network": "hedera:testnet", "amount": "100000", "asset": "0.0.0",
  "payTo": "0.0.1003", "maxTimeoutSeconds": 300, "extra": { "feePayer": "0.0.1004" } }
```

Every settlement is written to an HCS topic as an `x402.receipt.v1` message, so the payment
history is provable from the chain rather than from this service's logs:

```json
{ "type": "x402.receipt.v1", "network": "hedera:testnet", "chain": "local",
  "route": "/api/data/paid", "payer": "0.0.1002", "payTo": "0.0.1003", "asset": "0.0.0",
  "amount": "100000", "transaction": "0.0.1004@1789194954.914449937",
  "at": "2026-09-12T06:35:54.914Z" }
```

## The two rails

| `FACILITATOR_URL` | Settles on | `HEDERA_NETWORK` |
| --- | --- | --- |
| `http://127.0.0.1:4020` (default on local) | the `hanvil` chain in this workspace | `local` |
| `https://api.testnet.blocky402.com` (default on testnet) | Hedera testnet | `testnet` |

The local facilitator in `src/facilitator.ts` is **x402's own reference facilitator**, not a
reimplementation. The only thing changed is `buildHederaClient`, the extension point the
reference example itself uses, which returns
`Client.forNetwork({ [HANVIL_GRPC_URL]: AccountId.fromString("0.0.3") })`. Production does not
run it: on testnet the facilitator is Blocky402.

### Which CAIP-2 id the local rail uses

`hedera:testnet`, even locally. `@x402/hedera` hardcodes
`SUPPORTED_HEDERA_NETWORKS = ["hedera:mainnet", "hedera:testnet"]` and asserts against it in the
server scheme, the client signer and the facilitator, so `hedera:localnet` is refused with
`Unsupported Hedera network`. The id names the payment rail; the chain is named separately by
`nodeUrl`, which `createHederaClient` accepts as an override. Three places have to be pointed at
`hanvil` or they reach the real network instead:

- the client signer's `nodeUrl`,
- `createHederaVerifyPayerSignature({ mirrorNodeUrl })` — it reads the payer's public key off a
  mirror, and the wrong mirror reports a wrong key, so every partial signature is refused as
  `signature_invalid`,
- `createHederaPreflightTransfer({ mirrorNodeUrl })` — the balance check.

A receipt therefore records `chain` as well as `network`: without it a local receipt would claim
testnet for a payment that never left the machine.

## Not covered

- A native Rust facilitator. `src/facilitator.ts` needs Node, as `hanvil run` needs an agent CLI
  and `npx`. The `hanvil` node itself still opens no outbound socket.
- HTS-token pricing. HBAR (`0.0.0`) only, so there is no token association step. `@x402/hedera`
  supports tokens; this service does not configure them.
- The usage meter is in memory and resets with the process. The HCS topic is the durable record.
- Paying yourself. `PAY_TO` must differ from `HEDERA_ACCOUNT_ID`: a transfer that nets to zero is
  refused by the facilitator's preflight, and the service refuses the configuration at boot.
- Schemes other than `exact`, and x402 v1.
