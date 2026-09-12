# The testnet rail: this service only.
#
# On `HEDERA_NETWORK=testnet` the facilitator is Blocky402, not `src/facilitator.ts`, and the
# mirror is the public one — so nothing here needs hanvil. The same image on `local` would need
# the chain in the same network namespace, which is what `hanvil toll` does instead.
FROM node:20-slim

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
