# Via

Via is a Bitcoin layer 2 built from zkSync Era. This glossary covers its Bitcoin deposit flow.

## Language

**Bridge payment**: A Bitcoin transaction with at least one output to Via's configured bridge address.
_Avoid_: deposit when the message and payment have not passed the deposit checks.

**Deposit message**: The receiver and amount decoded from a bridge payment.
_Avoid_: accepted deposit before the amount and receiver checks pass.

**Receiver**: The L2 account identified by a deposit message.
_Avoid_: bridge address, which receives the Bitcoin payment.

**Accepted deposit**: A deposit message that meets Via's amount and receiver requirements for L2 execution.
Acceptance does not mean that execution has completed.
_Avoid_: credited deposit before execution.

**Credited deposit**: A deposit whose credit has been applied on L2.
_Avoid_: treating a stored deposit as credited without execution evidence.

**Rejected bridge payment**: A bridge payment that produces no accepted deposit after Via completes its deposit checks.
_Avoid_: rejected transaction, which could imply that Bitcoin rejected the payment.
