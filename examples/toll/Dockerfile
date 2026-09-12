# The testnet rail: this service only.
#
# On `HEDERA_NETWORK=testnet` the facilitator is Blocky402, not `src/facilitator.ts`, and the
# mirror is the public one — so nothing here needs hanvil. The same image on `local` would need
# the chain in the same network namespace, which is what `hanvil toll` does instead.
# Node 22, not 20: @x402/core pulls @solana/wallet-standard-features, whose engines field is
# ">=22". `yarn install` refuses on 20 with "Found incompatible module". package.json says
# ">=20" because the Hedera path works there; the x402 path does not.
FROM node:22-slim

WORKDIR /app

COPY package.json yarn.lock tsconfig.json ./
RUN yarn install --frozen-lockfile

COPY src ./src

# tsx is a devDependency; the image keeps it. A build step would buy a smaller image and one more
# thing to get wrong the night before a deadline.
ENV HEDERA_NETWORK=testnet
ENV PORT=4021
EXPOSE 4021

# HEDERA_ACCOUNT_ID, HEDERA_PRIVATE_KEY, PAY_TO and TOLL_TOPIC_ID come from the host's environment.
# config.ts throws and names the variable if one is missing; it never defaults silently.
CMD ["yarn", "start"]
