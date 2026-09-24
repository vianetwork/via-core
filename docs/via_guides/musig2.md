# MuSig2 Bridge Wallet

This module creates a **bridge address** that supports two spending methods:

1. **Key Path Spend (Key Hash)**

- Uses a MuSig2 aggregate public key.
- Requires **N-of-N signers** to jointly produce a valid signature.
- Primary purpose: **processing withdrawals**.

2. **Script Path Spend (Script Hash)**

- Uses an alternative script-based spending condition.
- Intended for **governance control**, allowing governance participants to transfer or reassign UTXOs if necessary.

This design provides both operational security (via MuSig2 key-path spending) and governance flexibility (via
script-path spending).

Governance spending does not bypass withdrawal accounting. A governance **payout** must use the shared `VIA_WI`
authorization and observation path, or have an explicit durable hold established before payout. The current
implementation does not census arbitrary payouts without metadata. A governance sweep alone is not fulfillment.
For recognized payments, script-path witness differences are allowed only when all remaining withdrawal
construction requirements, including sequence and fee-adjusted outputs, match.

Wallet rotation fails closed at the verifier watcher, including while signing is disabled; it requires separately
reviewed reconciliation, not resetting wallet identity or deleting old holds. See the [withdrawal cutover and
recovery guide](../../via_verifier/README.md#withdrawal-processing-and-coordinated-cutover). The example below
computes wallet parameters; it does not authorize activation, rotation or out-of-band payouts.

## Example

```sh
cargo run --example compute_musig2 -- \
  --signers 025b3c069378f860cc4dae864a491e0cd33cc559b9f82fc856d4dcc74d3d763241,03c2871e18d4fb503ead90461da747b40df5e28da0fd3e067f3731f1a28da60ddf \
  --governance-keys 025b3c069378f860cc4dae864a491e0cd33cc559b9f82fc856d4dcc74d3d763241,03c2871e18d4fb503ead90461da747b40df5e28da0fd3e067f3731f1a28da60ddf,03445c516584d751643442bea558be2c5d77a6c3377e86fe6e78e3b992dd68ac62 \
  --threshold 2 \
  --output my_wallet.json
```
