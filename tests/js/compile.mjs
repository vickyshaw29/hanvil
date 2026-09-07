// Compiles tests/fixtures/Counter.sol with solc-js into tests/fixtures/Counter.json.
// Run once after editing the contract: `node tests/js/compile.mjs`. The output is checked in so
// the Rust and JS tests need no compiler at test time.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import solc from "solc";

const here = dirname(fileURLToPath(import.meta.url));
const fixtures = join(here, "..", "fixtures");
const source = readFileSync(join(fixtures, "Counter.sol"), "utf8");

const input = {
  language: "Solidity",
  sources: { "Counter.sol": { content: source } },
  settings: {
    evmVersion: "cancun",
    optimizer: { enabled: true, runs: 200 },
    outputSelection: { "*": { "*": ["abi", "evm.bytecode.object", "evm.deployedBytecode.object"] } },
  },
};
const output = JSON.parse(solc.compile(JSON.stringify(input)));
const errors = (output.errors ?? []).filter((e) => e.severity === "error");
if (errors.length) {
  console.error(errors.map((e) => e.formattedMessage).join("\n"));
  process.exit(1);
}
const contract = output.contracts["Counter.sol"].Counter;
writeFileSync(
  join(fixtures, "Counter.json"),
  JSON.stringify(
    {
      solc: solc.version(),
      evmVersion: "cancun",
      abi: contract.abi,
      bytecode: "0x" + contract.evm.bytecode.object,
      deployedBytecode: "0x" + contract.evm.deployedBytecode.object,
    },
    null,
    2,
  ) + "\n",
);
console.log(`Counter.json written (solc ${solc.version()}, ${contract.evm.bytecode.object.length / 2} bytes of init code)`);
