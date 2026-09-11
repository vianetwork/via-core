# AGENTS.md — via_btc_ingestion

This crate is the **shared contract** for Bitcoin block ingestion: the types
and traits every ingestion engine and every role adapter (sequencer,
verifier, standalone indexer) compile against. The design is documented in
`docs/ingestion/ingestion-kernel.md`.

Before editing:

1. This crate must stay free of zksync and RPC-capable dependencies. Do not
   add either; that boundary is the point of the crate.
2. Any change to canonical encodings (`canonical_bytes`, `context_hash`,
   `wire_tag` values) changes plan identity. Bump the relevant version
   constant and refreeze the golden hash test; never change bytes silently.
3. Behavior changes must be reflected in the acceptance suite in
   `via_btc_ingestion_tests`, which is the judge for every implementation.

See root `AGENTS.md` → *Reuse and duplication discipline* and *Source
comment discipline*.
