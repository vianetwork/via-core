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

**Reader position**: The height and hash through which a Via node has committed Bitcoin observation. _Avoid_: scan group
position, which belongs to the superseded four-group design.

**Consumer progress**: The work a consumer has durably completed from the reader's observations. It may trail the reader
position. _Avoid_: reader position, when proof or other dependent work is meant.

**Fork point**: The last block that a Via node's stored chain shares with its authoritative Bitcoin node's chain after a
reorg. _Avoid_: last valid block, and reorg height, which names the first replaced block.

**Maturity depth**: The number of confirmations a Bitcoin block needs before a Via node takes an irreversible action,
such as a vote, finalization or payout, based on it. The block itself counts as the first confirmation. Each network
has its own. _Avoid_: block confirmations, finality depth, horizon.

**Confirmed frontier**: The furthest Bitcoin block whose history Via has durably protected against automatic removal.
It never moves backward. _Avoid_: current maturity height, which may decrease after a chain shrink.

**Reorg depth**: The number of old-branch blocks replaced by one reorg. _Avoid_: automatic recovery depth, which names a
recovery policy rather than the size of a reorg.

**Recovery barrier**: The durable hold on new authority while Bitcoin recovery, reconciliation or a fatal stop remains
unresolved. _Avoid_: proof gap, which can hold only dependent work.

### Recovering from reorgs

**Evidence**: An immutable record of what a Bitcoin transaction carried, such as a Via message and the transaction
that carried it. A reorg does not change evidence. _Avoid_: observation, when its acceptance is meant.

**Canonical membership**: The fact that a piece of evidence is in a given block of the current chain. A re-mined message
keeps its evidence and gets a new membership. _Avoid_: inclusion status, active flag, confirmed.

**Replay reconciliation**: Establishing that kept Via effects and historical facts agree with the Bitcoin history being
read again. _Avoid_: format upgrade, which alone says nothing about agreement.

**Rotation decision prefix**: The fixed Bitcoin history used to decide whether one verifier-set rotation is allowed.
_Avoid_: latest tip, whose later history cannot change a permanent decision.
