//! The harness: `hanvil run` and its siblings. A port of `hedera-harness` `dev` @ 587a2f3 that
//! drives a coding agent against the in-process chain. Module by module it follows the
//! TypeScript layout so a reader can diff them; `docs/code-plan.md` §16 has the contract.

// Removed with the commit that lands `hanvil run` (Fri 2026-09-11): until the attempt loop reads
// them, most recipe fields have no consumer. Tracked in plan.md §15.
#![allow(dead_code)]

pub(crate) mod spec;
