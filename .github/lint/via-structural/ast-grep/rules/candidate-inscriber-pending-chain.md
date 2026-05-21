# Candidate: Inscriber Pending-Chain UTXO Management

## Status

Candidate / Documentation only (no active .yml rule yet).

## Why This Area Is Risky

The `Inscriber` in `via_btc_client` maintains a persisted `fifo_queue: VecDeque<InscriptionRequest>` of unconfirmed commit/reveal pairs.

When preparing the next commit transaction (`prepare_commit_tx_input`):

- It walks the entire queue and builds a set of "spent" OutPoints:
  - All `spent_utxo` from previous commit transactions
  - Reveal fee-payer inputs
  - Reveal change outputs from all but the **head** of the queue
- It then filters live UTXOs and **only** re-introduces the head of the queue's reveal change output as spendable.

This creates a manual "pending UTXO chain" abstraction where:

- Position in the `fifo_queue` determines whether a change output is locked or spendable.
- `OutPoint` (txid + vout) of specific reveal change outputs carry conditional identity.
- The head change output (identified via `fee_payer_ctx`) is special.

## Related Identities (from IDENTITIES.tsv)

- `pending_inscription_fifo_queue`
- `pending_chain_utxo_outpoint`
- `fifo_queue_head_change_outpoint`
- `fee_payer_ctx`

## Current Protection

- Documented in the Via Agent Harness (`IDENTITIES.tsv`, future `btc-sender-watch.md` card).
- `get_balance()` also walks the queue to add pending outputs.
- Strong comment in the code warning that only the service should use the address.

## Potential Future Rule

A syntactic ast-grep rule is difficult because the pattern is mostly:

- Building a `HashMap<OutPoint, bool>` from a queue iteration
- Special-casing `queue.front()`
- Filtering + re-adding one specific change output

Possible lightweight approaches later:

- Flag direct iteration over `context.fifo_queue` combined with `spent_utxos.insert` on reveal change outputs.
- Flag use of fixed vout constants (`REVEAL_TX_CHANGE_OUTPUT_INDEX`) without clear comments.

For now the protection is **documentation + review** rather than automated structural rule.

## References

- `core/lib/via_btc_client/src/inscriber/mod.rs` (prepare_commit_tx_input, get_balance, sync_context_with_blockchain)
- `IDENTITIES.tsv` (new rows added 2026-05)
- `RISK_REGISTER.md` (risk.btc.pending_vs_trusted_balance_and_utxo_identity)
