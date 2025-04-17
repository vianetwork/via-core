# Protocol Upgrade Tool

## Introduction

The Protocol Upgrade Tool is a command-line utility that enables users to upgrade the protocol of a node.

## Usage

To generate a protocol upgrade proposal, follow the steps below:

1. Publish new system contracts and base system contracts
2. Prepare calldata for L2 upgrade
3. Generate the proposal transaction and execute it

### Create a Protocol Upgrade Proposal

To create a protocol upgrade proposal, use the following command:

```bash
yarn start upgrades create <upgrade-name> --protocol-version <protocol-version>
```

This command will create a folder named after the upgrade in the `etc/upgrades` directory. All necessary files for the
upgrade will be generated in this folder. The folder name follows the format: `<timestamp>-<upgrade-name>`.

Subsequent commands will use the latest upgrade located in the `etc/upgrades` folder. The latest upgrade is determined
based on the timestamp in the name.

The command also creates a common file with fields such as `name`, `protocolVersion`, and `timestamp`.

### Deploy New System Contracts and Base System Contracts

To publish bytecodes for new system contracts and base system contracts together, use the following command:

```bash
yarn start system-contracts publish \
--private-key <private-key> \
--l2rpc <l2rpc> \
--environment <environment> \
--bootloader \
--default-aa \
--system-contracts
```

The results will be saved in the `etc/upgrades/<upgrade-name>/<environment>/l2Upgrade.json` file.

Please note that publishing new system contracts will append to the existing file, while publishing them all together
will overwrite the file.

### Generate Proposal Transaction and Execute It

To generate a proposal transaction and execute it, combining all the data from the previous steps, use the following
command:

```bash
yarn start l2-transaction upgrade-system-contracts
  --environment <environment>
  --private-key <l1-private-key>
```
