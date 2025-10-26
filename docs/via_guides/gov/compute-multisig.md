# Compute a multisig wallet that requires M/N signers

1. Compute the multisig wallet with 2/3 signers, **the public keys should be coma separated**.

```sh,
via multisig compute-multisig \
    --pubkeys <public1,public2,public3> \
    --minimumSigners 2 \
    --outDir './multisig.json' \
    --network <mainnet | testnet | regtest>
```
