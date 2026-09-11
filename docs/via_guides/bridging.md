# Bitcoin deposits and payout signing

Via converts accepted deposits at its Bitcoin bridge into L2 priority transactions. The bridge wallet holds the BTC,
while the sequencer executes the transactions in the VM.

The [deposit encoding reference](../../core/lib/via_btc_client/DEV.md#deposit-decoding) describes the accepted
message fields. [L2 contract calls through Bitcoin](bridging-proposals.md) explains the earlier proposals for
contract calls and forced withdrawals.

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
