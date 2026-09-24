---
status: pending
---

# Synthetic bridge checks that prove value transfer

## Question and scope

What should a controlled deposit-and-withdrawal cycle prove, and how can it avoid spending repeatedly when the outcome is unknown? The proposed check is an independent client of the bridge, not a replacement for passive monitoring or signer authorization. No wallet was funded and no transaction, live RPC call, or infrastructure change was performed for this research.

Via is pinned to [`8a49f355bfe31720f21b8181db471243709194e5`](https://github.com/vianetwork/via-core/tree/8a49f355bfe31720f21b8181db471243709194e5). Local newer zkSync is pinned to [`ff5f519b11cff863edcfa0f75af10fea113806b0`](https://github.com/matter-labs/zksync-era/tree/ff5f519b11cff863edcfa0f75af10fea113806b0), not verified latest upstream. Repository examples establish available mechanisms, not a scheduled or deployed probe.

## What Via already provides

`infrastructure/via/src/token.ts` contains `deposit`, `depositWithOpReturn`, and `withdraw`. Deposit converts BTC to integer units, calls `estimateGasFee`, and adds that estimate before invoking a Bitcoin example. Withdrawal encodes the Bitcoin destination for `withdraw(bytes)`, submits the L2 transaction, waits for its receipt, and prints the L2 balance. It does not follow that request to a confirmed Bitcoin output. The [playground guide](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via-playground/README.md) is an interaction example; the actual [token commands](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/infrastructure/via/src/token.ts) define the behavior. They are not suitable as unattended custodial wrappers without separate key-handling and input controls.

The existing load generator is a closer implementation than an HTTP check. `AccountLifespan::execute_btc_deposit` calls `btc_deposit::deposit`, receives a Bitcoin hash, and returns `ReportLabel::done()` without checking the resulting canonical L2 transaction or credited value. `execute_withdraw` submits an L2 withdrawal through the normal transaction path. Thus its success labels do not establish an end-to-end Bitcoin payout. See [`core/tests/via_loadnext/src/account/tx_command_executor.rs`](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/tests/via_loadnext/src/account/tx_command_executor.rs#L104-L182). `SyncTransactionHandle::wait_for_finalize` concerns the L2 receipt and finalized block, and its timeout is optional; it is not an independent payout assertion. See the [transaction handle](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/tests/via_loadnext/src/sdk/operations/mod.rs).

The credit expectation must come from the Via protocol, not from an observed balance increase alone. `ViaL1Deposit::value` multiplies satoshis by `MANTISSA`; `is_valid_deposit` requires enough value for `GAS_LIMIT * MAX_FEE_PER_GAS`. Conversion to `L1Tx` assigns `to_mint`, `refund_recipient`, and a `priority_id` derived from Bitcoin position. The amount minted is not automatically the receiver's net balance delta after execution fees. See [`core/lib/types/src/l1/via_l1.rs`](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/core/lib/types/src/l1/via_l1.rs).

Withdrawal correlation requires more than a destination address.
`WithdrawalRequest` includes `id`, `l2_tx_hash`, `l2_tx_log_index`, `receiver`, and `amount`.
The type module also defines `group_withdrawals_by_address`, but the inspected [withdrawal session][withdrawal-session] creates one output per selected request.
The helper's existence does not establish that the active payout path groups recipients.
The DAL stores `via_withdrawals` separately from `via_bridge_withdrawals`, whose `tx_id` identifies the Bitcoin payment transaction.
These records can aid diagnosis but cannot replace the independently decoded Bitcoin output.
See the [type definition](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/lib/via_verifier_types/src/withdrawal.rs) and [DAL](https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/lib/verifier_dal/src/withdrawals_dal.rs).

## Real comparative mechanisms

Local newer zkSync has executable integration flows rather than a Via-compatible funded monitor. `ether.test.ts` submits `alice.withdraw`, waits for finalization, and calls `alice.finalizeWithdrawal`. It explicitly leaves the L1 ETH balance assertion unsupported in that path. A passing receipt test therefore cannot be treated as proof of recipient value. `shouldChangeETHBalances` and `shouldChangeTokenBalances` show where value assertions belong, but their Ethereum fee and bridge semantics are not Via's Bitcoin fee policy. See [newer ETH tests](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/tests/ts-integration/tests/ether.test.ts#L245-L272) and [base-token tests](https://github.com/matter-labs/zksync-era/blob/ff5f519b11cff863edcfa0f75af10fea113806b0/core/tests/ts-integration/tests/base-token.test.ts).

The separate first-party [`matter-labs/era-watchdog` at `025b1ed28f0eca77825488effc802390f11f582e`](https://github.com/matter-labs/era-watchdog/tree/025b1ed28f0eca77825488effc802390f11f582e) is an actual periodic synthetic service. Its `DepositBaseFlow.getDepositRequest` deposits one base-token unit to the probe wallet. `getLastExecution` scans a bounded `MAX_LOGS_BLOCKS` range, derives the L2 hash through `getL2HashFromPriorityOp`, and distinguishes unknown history from failed or successful L2 execution. `DepositFlow.executeWatchdogDeposit` estimates gas, skips when `DEPOSIT_L1_GAS_PRICE_LIMIT_GWEI` is exceeded, and records individual steps. `run` uses a bounded retry count. See [`depositBase.ts`](https://github.com/matter-labs/era-watchdog/blob/025b1ed28f0eca77825488effc802390f11f582e/src/depositBase.ts) and [`deposit.ts`](https://github.com/matter-labs/era-watchdog/blob/025b1ed28f0eca77825488effc802390f11f582e/src/deposit.ts).

This service supplies useful scheduling and status conventions, not a complete Via probe. Its `WithdrawalFinalizeFlow` calls `fetchFinalization`, `isWithdrawalFinalized`, and `estimateFinalization`. The legacy path calls `finalizeWithdrawal.estimateGas`. Neither path sends the finalization payment. The distinction matters: a successful simulation is not a paid withdrawal. The service also contains a protocol override for `legacy-withdrawal` versus `interop-bundle`, because L1 contract and L2 registered versions may differ. That is direct evidence that a re-fork must bind the client to actual settlement interfaces, not only a release label. See [`withdrawalFinalize.ts`](https://github.com/matter-labs/era-watchdog/blob/025b1ed28f0eca77825488effc802390f11f582e/src/withdrawalFinalize.ts).

Optimism offers a stronger value assertion in `RunWithdrawalsTest`. It deposits, checks the L2 balance increase, submits a withdrawal, accounts for L2 gas, invokes `ProveAndFinalizeWithdrawal`, and checks the L1 balance change after adding back prove, finalize, and dispute-resolution fees. This is a test implementation, not evidence of a deployed production canary. Its useful contribution is the accounting invariant; its dispute games and Ethereum transaction fees do not apply to Via. Source: [`optimism` at `33fbe016b879d3c4a5187516cbab7668b4527627`, `op-e2e/system/bridge/withdrawal.go`](https://github.com/ethereum-optimism/optimism/blob/33fbe016b879d3c4a5187516cbab7668b4527627/op-e2e/system/bridge/withdrawal.go).

Prometheus Blackbox Exporter remains appropriate for DNS, HTTP, TCP, and RPC transport reachability. `probe_success` means its configured transport probe passed. It neither correlates deposit credit nor owns transaction history. Reusing an HTTP readiness check as the funded probe is therefore rejected. See [`Probers` and `Handler`](https://github.com/prometheus/blackbox_exporter/blob/46ea0224f57b708fdff317f8872765a77ef64034/prober/handler.go).

## Proposed proof and spending contract

A useful cycle records a durable run identity before spending, the network and protocol configuration, and the intended receiver and amount. Its states distinguish prepared, deposit broadcast, deposit confirmed, credit verified, withdrawal submitted, payout observed, payout confirmed, and complete. Unknown outcome, timeout, policy skip, and operator pause are separate states, not successful completion. These are proposed probe states, not existing Via fields.

The minimum successful cycle establishes all of the following:

- The intended Bitcoin deposit output and accepted deposit carrier belong to the selected chain and bridge configuration. Its block hash and transaction position remain recorded through the chosen confirmation depth.
- The correlated L2 priority transaction executed successfully for the intended account. The receiver's balance delta agrees with the deposit's converted value and independently calculated execution-fee effects. An isolated receiver avoids unrelated transfers masking a discrepancy.
- The L2 withdrawal receipt and event identify the requested value and Bitcoin destination. The expected payout is fixed from that request and the approved withdrawal-fee policy before observing the Bitcoin output. The probe must not derive its expected value from the payout it is testing.
- The correlated Bitcoin transaction pays the expected amount to the intended script, net of only the approved fee allocation. The output reaches the chosen confirmation depth. A later reorg retracts the success state until reconfirmation.

Retries of reads may use bounded backoff. A timeout after broadcast is an unknown transaction outcome, not permission to create another deposit or withdrawal. Resume uses the stored Bitcoin transaction identity and any explicit replacement lineage, or the L2 sender nonce and transaction hash. A per-wallet single-flight rule prevents concurrent cycles from racing UTXOs or nonces. Restart recovery must reconcile the previous run before admitting a new funded run.

Bitcoin Core v27.0 offers a useful limit on abandonment analogies. Its
[`abandontransaction` help][bitcoin-abandon-rpc] says abandonment permits reuse of inputs from the
transaction and its in-wallet descendants, but applies only outside a block and the local mempool.
[`AbandonTransaction`][bitcoin-abandon-state] also documents that those states are not permanent:
later mempool admission or confirmation can change them.

Local absence is therefore not proof that a broadcast failed permanently. This wallet behavior does
not define Via's hold-release policy, and abandonment is not a universal prerequisite for spending
inputs again. A probe must preserve an unknown outcome until its own reconciliation policy resolves it.

A small principal alone does not bound spending. The owner must cap total fees per attempt, replacement fees, aggregate fees per day, outstanding principal, and cumulative unreconciled loss. A fee-rate cap is not a total-cost cap. The wallet must have only probe funds, no bridge signing authority, no automatic unlimited refill, and no private keys in command arguments or logs. Sweeping residual funds is another authorized spend, not automatic cleanup of an uncertain run.

Cadence and deadlines require network-specific choices. Bitcoin block arrival, fee congestion, proof production, settlement, and bridge batching can each delay a healthy cycle. Record the failing stage and distinguish a value mismatch from a liveness timeout, insufficient probe funds, RPC disagreement, and an intentionally skipped expensive run. Repeated skips need their own coverage-age signal. Retries must never make the last successful cycle appear newer.

## Future re-fork and human choices

The Via-owned contract is the expected deposit credit, withdrawal value after the approved fee policy, destination, durable correlation, confirmation policy, and spending limits. The SDK, polling API, receipt type, database diagnostic queries, and scheduler are adapters. `ViaL1Deposit`, `WithdrawalRequest`, the Bitcoin parser, and the withdrawal builder remain authoritative Via dependencies; newer zkSync transaction finalization is not equivalent to Bitcoin payout.

A future client can reuse newer zkSync's L2 provider and receipt conventions only after replay proves the mapping. Required golden cases include successful credit with fees, multiple outputs or withdrawals to one destination, lost broadcast response, restart after submission, replacement transaction, reorg after tentative success, insufficient funding, and a long settlement delay. Store exact transaction bytes, block identities, receipt and event evidence, fee-policy version, and expected versus observed amounts. Tests and golden cases described here were not executed.

The operator must choose the repository and accountable maintainer, network, custody method, principal and fee budgets, cadence, confirmation depths, stage deadlines, payout-fee assertion, incident route, and residual-fund policy. A service outside the node deployment is preferable for independence, but it may still share an RPC provider or notification dependency; those shared dependencies must be stated. Separately approved funded execution is required to prove activation. Source publication alone establishes neither a recurring schedule nor a complete value-transfer check.

## Durable execution and accounting choices

This proposal covers B1 through B5. The probe is a separate client process, not a new sender task.
Keep the protocol adapter near the Via code that defines deposits and withdrawals. Let the
platform owner package and schedule the process independently. A separate service repository is
reasonable if it already has an accountable maintainer and release process. Source research
cannot assign that person, authorize a network, or grant custody.

Bitcoin Core v27.0 provides a useful prepare-before-broadcast interface.
[`walletcreatefundedpsbt`](https://github.com/bitcoin/bitcoin/blob/d82283950f5ff3b2116e705f931c6e89e5fdd0be/src/wallet/rpc/spend.cpp)
returns `psbt`, `fee`, and `changepos`. Its `lockUnspents` option locks selected outputs,
while `subtractFeeFromOutputs` changes recipient amounts. `walletprocesspsbt` returns `complete`
and, when complete, `hex`. `FinishTransaction` distinguishes returning transaction bytes from
calling `CommitTransaction`. This separation is useful for a probe that must durably record the
signed transaction and its identity before an uncertain broadcast. Wallet locks alone do not
provide the probe's restart reconciliation or cumulative spending budget.

Use a dedicated low-balance wallet, an isolated L2 account, and one outstanding cycle per wallet.
A hot probe key permits unattended operation but must not control bridge funds. An external
signer limits key exposure but adds signer availability to the check. Start without automatic
refill or automatic fee replacement. Add either only after the operator approves explicit limits.
The deposit must retain its intended bridge value, so blindly subtracting fees from that output
would change the assertion.

Proposed durable fields are `cycle_id`, `network_id`, `protocol_config_hash`, `fee_policy_id`,
`stage`, `started_at`, `cycle_deadline`, `stage_deadline`, `deposit_raw_tx`, `deposit_txid`,
`deposit_vout`, `deposit_block_hash`, `l2_priority_tx_hash`, `withdrawal_tx_hash`,
`withdrawal_sender_nonce`, `withdrawal_log_index`, `expected_gross_sats`,
`expected_fee_sats`, `expected_net_sats`, `payout_txid`, `payout_vout`, and
`payout_block_hash`. Preserve every authorized replacement hash rather than overwrite the original.
These are proposed application fields, not existing Via schema.

A small transactional journal is sufficient for one process and one wallet. It must commit the
cycle, reservations, exact signed bytes, and the next permissible action before network submission.
Multiple active runners require shared atomic admission and fencing before signing. An expiring
lease alone cannot stop an old runner from broadcasting already signed bytes.

LND at `d72a3aaf261e278fa4aad5be4453df2f74ab50ee` separates a payment from its attempts.
[`RegisterAttempt`](https://github.com/lightningnetwork/lnd/blob/d72a3aaf261e278fa4aad5be4453df2f74ab50ee/channeldb/payment_control.go)
accounts for in-flight amounts in the same database batch that records the attempt.
[`decidePaymentStatus`](https://github.com/lightningnetwork/lnd/blob/d72a3aaf261e278fa4aad5be4453df2f74ab50ee/channeldb/payment_status.go)
derives status from unresolved, settled, and failed attempt facts. For Via, derived cycle status is
one viable design; a stored status with atomic transition guards and reconciliation is another.
The required invariant is that an unresolved external effect remains visible to admission after
restart. The comparison does not require a table per stage or prohibit a guarded status column.

Reserve both principal and maximum authorized fees before admitting a cycle. Record actual Bitcoin
fees as input value minus all output values, and distinguish change from spent principal. Record
L2 execution fees separately in their native integer units. Admission must consider fee reservations
for unresolved attempts, daily spend, outstanding principal, replacement allowance, and
unreconciled loss. A restart or UTC-day boundary must not release unresolved reservations.
The owner must define the daily window and the limits. A fee-rate setting alone enforces none
of those aggregate limits.

Prefer read retries and exact-byte rebroadcast over creating another transaction. After a lost
response, reconcile the stored hash and replacement lineage. Preserve the L2 sender nonce and
signed transaction for the same reason. A stage timeout stops new spending and pages the owner.
It does not erase transaction identity or release its reserved budget. The cycle deadline is fixed
at admission and does not move when a step retries. Chain inclusion must retain block hashes:
confirmation count alone cannot identify which prior success a reorg invalidated.

The minimum alert contract separates value mismatch, overdue stage, unknown broadcast outcome,
insufficient probe funds, policy skip, and old successful coverage. Proposed exported values are
`via_bridge_probe_last_success_timestamp_seconds` and
`via_bridge_probe_cycle_started_timestamp_seconds`; a bounded stage label supplies diagnosis.
Cycle IDs and transaction hashes belong in the journal and incident evidence, not metric labels.
Skipped or retried work never updates last success.

The withdrawal contract determines the permitted payout-fee assertion and request correlation.
The probe consumes that contract; it cannot choose a different fee rule to make its observed
payout pass. A local simulated lifecycle can prove restart and budget behavior, but only separately
approved funded execution can prove the selected network's full credit-and-payout path. Neither
proof was run here.

[withdrawal-session]: https://github.com/vianetwork/via-core/blob/8a49f355bfe31720f21b8181db471243709194e5/via_verifier/node/via_verifier_coordinator/src/sessions/withdrawal.rs#L84-L96
[bitcoin-abandon-rpc]: https://github.com/bitcoin/bitcoin/blob/d82283950f5ff3b2116e705f931c6e89e5fdd0be/src/wallet/rpc/transactions.cpp#L799-L807
[bitcoin-abandon-state]: https://github.com/bitcoin/bitcoin/blob/d82283950f5ff3b2116e705f931c6e89e5fdd0be/src/wallet/wallet.cpp#L1278-L1327
