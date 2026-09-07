# Via External Node

This application is a read replica that can sync from the main node and serve the state locally.

Note: this README is under construction.

## Settlement detail APIs

The EN serves `zks_getL1BatchDetails`, `zks_getBlockDetails`, and `zks_getTransactionDetails` from the local mirror
maintained by `BatchStatusUpdater`. Main nodes read Bitcoin inscription tables. `use_synced_settlement` selects only
detail readers, not transaction submission or proxying. RPC Bitcoin hashes are already in display order and are not
reversed again.

### Acceptance and health

`viaIsFinalized` is `true` for acceptance, `false` for rejection, and absent for an unknown/unsupported verdict (NULL).
Only TRUE authorizes Via execution. The EN does not persist negative verdicts: rejected and unknown batches remain
unexecuted, with the field absent.

Accepted batches share the synthetic `0x11…11` hash. `l1_batches.via_en_executed_at` stores each batch's main-node
verifier-finalization time, not the shared transaction history's time.

Without affirmative acceptance for that sentinel, the EN keeps running and commit/proof sync continues.
`batch_status_updater` health becomes `affected`, with cursors, `via_execution_blocked_at`, and an `error`. Health
publishes only when cursors or the blocked batch change. RPC failures retain the blocked state; a successful fetch with
no unsupported execution (including a missing batch) clears it. Execution cannot skip rejected/unverdicted batches.
Malformed accepted timestamps and database errors remain fatal.

### Upgrade and historical-data boundary

Apply the additive `via_en_execution_timestamp` migration through the approved process and upgrade main before EN for
uninterrupted execution mirroring. An older/rolled-back main stalls execution, not the EN. There is no backfill: legacy
rows retain commit/proof but have NULL execution times and withheld execution in detail APIs. Updater cursors do not
revisit them. Recovery is a separate approved, bounded revalidation against an upgraded authoritative source; do not
infer missing times or acceptance from shared history.

The ETH-backed finalized tag can still count legacy links hidden by detail APIs; it does not prove recovery. The
separate main-node finalized-tag and explorer shared-hash timestamp issues are unchanged.

## Local development

This section describes how to run the external node locally

### Configuration

Right now, external node requires all the configuration parameters that are required for the main node. It also has one
unique parameter: `API_WEB3_JSON_RPC_MAIN_NODE_URL` -- the address of the main node to fetch the state from.

The easiest way to see everything that is used is to compile the `ext-node` config and see the contents of the resulting
`.env` file.

Note: not all the config values from the main node are actually used, so this is temporary, and in the future external
node would require a much smaller set of config variables.

To change the configuration, edit the `etc/env/chains/ext-node.toml`, add the overrides from the `base` config if you
need any. Remove `etc/env/chains/ext-node.env`, if it exists. On the next launch of the external node, new config would
be compiled and will be written to the `etc/env/chains/ext-node.env` file.

### Running

To run the binary:

```sh
ZKSYNC_ENV=ext-node zk f cargo run --release --bin zksync_external_node
```

### Clearing the state

This command will reset the Postgres and RocksDB databases used by the external node:

```sh
ZKSYNC_ENV=ext-node zk db reset && rm -r $ZKSYNC_HOME/en_db
```

## Building & pushing

Use the `External Node - Build & push docker image` GitHub action. By default, it'll publish the image with `latest2.0`
tag.
