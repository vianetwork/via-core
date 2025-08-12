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

## Example

```sh
cargo run --example compute_musig2 -- \
  --signers 025b3c069378f860cc4dae864a491e0cd33cc559b9f82fc856d4dcc74d3d763241,03c2871e18d4fb503ead90461da747b40df5e28da0fd3e067f3731f1a28da60ddf \
  --governance-keys 025b3c069378f860cc4dae864a491e0cd33cc559b9f82fc856d4dcc74d3d763241,03c2871e18d4fb503ead90461da747b40df5e28da0fd3e067f3731f1a28da60ddf,03445c516584d751643442bea558be2c5d77a6c3377e86fe6e78e3b992dd68ac62 \
  --threshold 2 \
  --output my_wallet.json
```
