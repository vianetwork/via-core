# Via

Via is a Bitcoin layer 2 built from zkSync Era. This glossary covers its Bitcoin deposit flow and how it reads Bitcoin.

## Language

### Deposits

**Bridge payment**: A Bitcoin transaction with at least one output to Via's configured bridge address. _Avoid_: deposit
when the message and payment have not passed the deposit checks.

**Deposit message**: The receiver and amount decoded from a bridge payment. _Avoid_: accepted deposit before the amount
and receiver checks pass.

**Receiver**: The L2 account identified by a deposit message. _Avoid_: bridge address, which receives the Bitcoin
payment.

**Accepted deposit**: A deposit message that meets Via's amount and receiver requirements for L2 execution. Acceptance
does not mean that execution has completed. _Avoid_: credited deposit before execution.

**Credited deposit**: A deposit whose credit has been applied on L2. _Avoid_: treating a stored deposit as credited
without execution evidence.

**Rejected bridge payment**: A bridge payment that produces no accepted deposit after Via completes its deposit checks.
_Avoid_: rejected transaction, which could imply that Bitcoin rejected the payment.

### Reading Bitcoin

**Bitcoin reader**: The one part of a Via node that reads Bitcoin blocks in order, detects reorgs and recovers from
them. _Avoid_: watcher, reorg detector, ordering owner.

**Authoritative Bitcoin node**: The one Bitcoin Core node whose view of the chain a Via component accepts. _Avoid_:
backend, provider, RPC endpoint.

**Scan group**: A set of Via processors that commit their effects and one scan position together. _Avoid_: consumer,
consumer group.

**Scan position**: The height and hash of the last Bitcoin block that a scan group has fully processed. Each scan group
in each Via node keeps its own. _Avoid_: cursor, checkpoint, bookmark, last processed block.

**Fork point**: The last block that a Via node's stored chain shares with its authoritative Bitcoin node's chain after a
reorg. _Avoid_: last valid block, and reorg height, which names the first replaced block.

**Maturity depth**: The number of confirmations a Bitcoin block needs before a Via node takes an irreversible action,
such as a vote, finalization or payout, based on it. The block itself counts as the first confirmation. Each network
has its own. _Avoid_: block confirmations, finality depth, horizon.

**Rollback-search limit**: The largest number of stored blocks a Bitcoin reader walks back to find a fork point before
it stops recovery and waits for an operator. _Avoid_: reorg window, recovery bound, and repair boundary, which names
what recovery may undo.

**Recovery barrier**: The committed record that marks a reorg recovery as in progress, from before its first destructive
step until all affected stores agree. _Avoid_: reorg flag, pause signal, proof gap, which is a separate stop for proofs
only.

### Recovering from reorgs

**Evidence**: An immutable record of what a Bitcoin transaction carried, such as a Via message and the transaction
that carried it. A reorg does not change evidence. _Avoid_: observation, when its acceptance is meant.

**Canonical membership**: The fact that a piece of evidence is in a given block of the current chain. A re-mined message
keeps its evidence and gets a new membership. _Avoid_: inclusion status, active flag, confirmed.

**Derived state**: State that a Via node computes from evidence with canonical membership, such as the current bridge
wallets or the queue of unexecuted deposits. _Avoid_: projection, and evidence, which recovery never rebuilds.

**Read-only inspection**: A run that reports what reorg recovery would change, without writing to any store and without
any component that signs, sends or coordinates. _Avoid_: dry-run, and mutation rehearsal, which writes.

**Mutation rehearsal**: A run of the real recovery writes against a disposable copy of a Via database. _Avoid_: dry-run,
read-only inspection.
