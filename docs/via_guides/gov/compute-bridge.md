# Create a MuSig2 Bridge Wallet

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
  --signers <bridge-signer-public-key-1,bridge-signer-public-key-2,bridge-signer-public-key-3> \
  --governance-keys <gov-signer-public-key-1,gov-signer-public-key-2,gov-signer-public-key-3> \
  --threshold 2 \
  --output new_bridge_address.json
```
