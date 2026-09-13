# Bitcoin deposits and payout signing

Via is a Bitcoin layer 2 built from zkSync Era. Bitcoin holds the bridge funds and records deposit messages.
Via executes transactions in its own virtual machine, or VM. Bitcoin is the base network, L1, and Via is L2.

To move BTC into Via, a user pays the bridge address on Bitcoin and supplies an L2 receiver address.
If the deposit passes Via's checks, it becomes an L2 priority transaction for that receiver.
The bridge wallet holds the BTC, while the sequencer executes the transaction in the VM.
The payment also funds execution fees, so its amount alone does not determine the receiver's final balance increase.

The [deposit encoding reference](../../core/lib/via_btc_client/DEV.md#deposit-decoding) describes the accepted
message fields. The [domain glossary](../../CONTEXT.md) distinguishes a bridge payment, a deposit message,
an accepted deposit, and a rejected bridge payment. [L2 contract calls through Bitcoin](bridging-proposals.md)
explains the earlier proposals for contract calls and forced withdrawals.

## Why a bridge payment needs a note

A Bitcoin payment tells Via that BTC went to the bridge. It does not identify the account that should receive L2 credit.
The depositor supplies that account in a note. Via uses a 20-byte L2 address, the same address format as Ethereum.

The parser supports two ways to carry the note:

- An OP_RETURN output is unspendable and carries data. Via reads the receiver from the first 20 bytes of its
  first data push. The encoding reference defines the accepted forms and ignored data.
- An inscription puts data in a taproot script branch guarded by `OP_FALSE OP_IF`. The branch does not execute.
  Via reads the enclosed data from the transaction witness, which supplies data for a Bitcoin spend.
  [Ordinals use the same envelope pattern](https://docs.ordinals.com/inscriptions.html).

Anyone can pay the bridge and choose the note's bytes. Bitcoin can accept a transaction whose note makes no sense
to Via. A bridge payment therefore needs further checks before it can become an accepted deposit.

Three kinds of scanner share the same `MessageParser`:

| Scanner | Deposit role |
| --- | --- |
| `BtcWatch`, in the main node and the external node | Creates L2 priority transactions |
| `VerifierBtcWatch` | Records deposits independently to check the sequencer |
| `L1Indexer`, the public indexer | Stores deposits for explorers |

## How a Bitcoin payment becomes an L2 transaction

The [main-node Bitcoin watcher](../../core/node/via_btc_watch/src/lib.rs) reads Bitcoin blocks and passes their
transactions to the shared `BitcoinInscriptionIndexer`. A transaction that pays the configured bridge address can
carry a deposit message in an inscription or an OP_RETURN output.

[`MessageParser`](../../core/lib/via_btc_client/src/indexer/parser.rs) reads the L2 receiver from the message and
the amount from a matching bridge output. The shared
[`BitcoinInscriptionIndexer`](../../core/lib/via_btc_client/src/indexer/mod.rs) checks that the decoded amount
equals the total paid to the bridge in the transaction. The message does not supply an independent amount to mint.

The [deposit processor](../../core/node/via_btc_watch/src/message_processors/l1_to_l2.rs) builds a `ViaL1Deposit`
from the message. It calls `ViaL1Deposit::l1_tx` to validate the deposit and convert it into an `L1Tx`.
The processor records valid transactions through `via_transactions_dal`.
The [mempool actor](../../core/node/via_state_keeper/src/mempool_actor.rs) loads pending transactions for the
[state keeper](../../core/node/via_state_keeper/src/io/mempool.rs), which selects them for execution.

This conversion lets Via reuse the L1 transaction execution path inherited from zkSync Era. Here, an `L1Tx`
represents a Bitcoin deposit for execution on L2.

## Amount, fees, and execution fields

[`ViaL1Deposit::l1_tx`](../../core/lib/types/src/l1/via_l1.rs) rejects receivers in the reserved system-address range
and payments that cannot cover the deposit gas budget. The conversion scales satoshis into the VM's base-token
units.

The resulting `L1Tx` puts the scaled payment in `to_mint` and uses the receiver as `refund_recipient`. Its execution
destination is also the receiver, while its call value is zero and its call data is empty. The payment therefore
funds the L1 transaction, including execution costs. The Bitcoin payment amount alone does not establish the
receiver's final balance increase.

An inscription contains `l2_contract_address` and `call_data`, but the conversion does not use those fields in the
resulting transaction. A nonzero contract field or nonempty data field does not activate the proposed contract-call
behavior. OP_RETURN deposits encode only a receiver.

## The verifier records deposits independently

The [verifier's deposit processor](../../via_verifier/node/via_btc_watch/src/message_processors/l1_to_l2.rs) uses
the same parser and `ViaL1Deposit` conversion. It stores the canonical transaction hash for comparison with L2
execution logs. Before writing to their databases, both deposit processors reverse the bytes of `common.tx_id`.

[`ViaPriorityOpId`](../../core/lib/types/src/l1/priority_id.rs) derives the deposit order from its Bitcoin block
height, transaction index, and output index. These identifiers are not consecutive queue counters.
[`verify_op_priority_id`](../../via_verifier/node/via_zk_verifier/src/lib.rs) compares a batch's deposit log keys
with pending canonical transaction hashes in that order. This checks the order of processed deposits. The check
does not set a deadline for including a pending deposit.

The verifier and the [separate indexer](../../via_indexer/node/indexer/src/message_processors/deposit.rs) also
store the inscription's call data. That stored data does not change the empty call data used for L2 execution.

## Bitcoin payouts require bridge signatures

A withdrawal requires a Bitcoin transaction that spends bridge funds to the withdrawal recipient.
[`WithdrawalSession`](../../via_verifier/node/via_verifier_coordinator/src/sessions/withdrawal.rs) prepares payouts
from withdrawal requests and validates the proposed transactions. The
[verifier coordinator](../../via_verifier/node/via_verifier_coordinator/src/verifier/mod.rs) participates in signing
those transactions under the [bridge wallet's spending conditions](musig2.md).

The [forced-withdrawal proposal](bridging-proposals.md#l2-execution-does-not-guarantee-bitcoin-payout) explains why
forced L2 execution alone does not guarantee a Bitcoin payout.

## Invalid messages and incomplete processing

If a deposit address fails the length checks in the encoding reference, the parser produces no message for that
encoding. The Bitcoin payment remains in place. The parser preserves independently decoded messages.

RPC and database failures have a different effect. The main-node watcher, the
[verifier watcher](../../via_verifier/node/via_btc_watch/src/lib.rs), and the
[separate indexer](../../via_indexer/node/indexer/src/lib.rs) save their last processed block after message
processing succeeds. A propagated failure leaves the range unfinished for a later retry.

### Why an invalid note can block every restart

Each scanner saves a cursor, the last Bitcoin block for which message processing succeeded.
The cursor lets the scanner resume after a restart without repeating all previous work.

Before [PR #390](https://github.com/vianetwork/via-core/pull/390), the OP_RETURN decoder used a Rust range slice
to read 20 receiver bytes. A 19-byte body that reached that slice caused a panic because the requested range did
not exist. The inscription decoder had the same kind of failure for a receiver or contract field of any length
other than 20 bytes. On these paths, a panic ended the scanner task.

The panic happened before the cursor advanced. After a restart, the scanner fetched the same block, read the same
note, and panicked again. Later Bitcoin blocks remained unprocessed. This is a poison pill: an input that causes
every attempt to process it to fail.

The address-length checks now reject the affected encoding. An independently valid companion message remains
eligible, and the scanner can process later transactions. Once its message processors succeed, it can save the
new cursor. The scanner does not reverse or refund the Bitcoin payment.

The old OP_RETURN failure was reproduced across restarts in isolated Bitcoin regtest and PostgreSQL fixtures.
The PR records that evidence separately from the parser and block tests for the inscription fix.
Those tests cover the changed paths, not every possible malformed Bitcoin message.

### Why zkSync Era treats an invalid event differently

The upstream [zkSync Era watcher][era-watcher] treats a failure to decode a priority-operation event as fatal.
Its [event client][era-client] filters logs to configured protocol-contract addresses. Anyone may call those
contracts, but the contract code controls the event format. A decode failure signals a protocol or node problem.
The watcher stops instead of silently skipping a protocol event.

Via reads deposit notes whose bytes come from Bitcoin senders.
Because senders can supply invalid notes, an address-length failure must not prevent progress through the block.
A propagated RPC or database failure still means that the scanner could not finish its work.

The [deposit rejection ADR](../adr/0002-reject-invalid-deposit-address-lengths.md) records this distinction and
why the panic fix preserves the existing interpretation of successfully decoded addresses.

[era-watcher]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cf/core/node/eth_watch/src/lib.rs
[era-client]: https://github.com/matter-labs/zksync-era/blob/ff5f519b11cf/core/node/eth_watch/src/client.rs
