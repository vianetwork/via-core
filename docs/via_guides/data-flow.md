Data Flow in the Via Protocol

Entry Point:
- L2 Transaction
- L1 -> L2 Transaction (via_btc_watch)
- L2 -> L1 Transaction

mempool

> db as communication tools between services

L1 Batch Commitment:
- via_state_keeper
- via_da_dispatcher
- via_btc_sender

L1 Batch Proving:
- prover gateway
- prover
- data bucket
- via_da_dispatcher
- via_btc_sender

L1 Batch Verification (finalization):
- via_btc_watch
- via_da_dispatcher
- via_zk_verification
- via_btc_sender

L1 Batch Execution:
- via_btc_watch
- coordinator <=> verifier
- withdrawal_builder
- musig2 coordination
- broadcast execution

