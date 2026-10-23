# Preserve admitted signing evidence across bounded HTTP upgrades

New withdrawal proposals omit full parents, and admission bounds the entire future authenticated response before
exposure. Compact HTTP uses Base64 and one admitted proposal; transport changes preserve the existing authentication
preimage, admitted bincode, public recovery records and signing-risk holds.

Mixed versions use unconditional dual readers, default-disabled compact writes and a capability offer in the signed
request target. Every new round must fit both legacy and compact transports until legacy removal; accepting compact-only
capacity earlier could strand a required old signer. Remove the temporary HTTP path only after the configured, offline,
standby and rollback fleet is capable, legacy work has drained, and active/public recovery evidence is compact-servable
or explicitly reconciled with holds preserved. Keep persisted decoders after HTTP retirement.

[Research and compatibility evidence](../research/bounded-signing-transport.md) records the donor fit, exact limits,
failure behavior and removal condition. Source readiness leaves deployment and signing activation under their existing
approval.
