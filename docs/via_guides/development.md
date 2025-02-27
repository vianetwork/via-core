# Development guide

This document covers development-related actions in Via.

## Initializing the project

Setup the `VIA_HOME` and `PATH` environment variables:

```
export VIA_HOME=path_to/via-core

export PATH=$VIA_HOME/bin:$PATH
```

After making these changes, restart your terminal or open a new one to apply them (or run `source ~/.bashrc` or
`source ~/.zshrc`).

To setup the main toolkit, `via`, run the following commands:

```
cd $VIA_HOME

via
```

You may also configure autocompletion for your shell via:

```
via completion install
```

📘 Note: The `via` cli depends on Docker, so please ensure that Docker is installed on your system.

Use Makefile to run all setup steps:

`make` - runs a help command to list all available options. `make via` - runs the basic VIA setup workflow (without
local Bitcoin explorer). `make all` - runs the full VIA setup workflow (with local Bitcoin explorer).

Most often you will want to run `make via` command that will do the following:

- Generate `$VIA_HOME/etc/env/target/via.env` file with settings for the applications.
- Initialize docker containers for local development (`bitcoind`, `bitcoin-cli`, `postgres`, `celestia-node`).
- Download and unpack files for cryptographical backend.
- Generate required smart contracts.
- Compile all required smart contracts.
- Deploy L2 system contracts.
- Generating random Bitcoin transactions.
- Obtain Celestia node address and auth token.
- Perform system bootstrap and update env file with bootstrap transaction IDs.
- Create “genesis block” for server.
- Run sequencer node.

Initializing may take pretty long, but many steps (such as downloading & unpacking keys and initializing containers) are
required to be done only once.

Usually, it is a good idea to do `make base` once after each merge to the `main` branch (as application setup may
change).

Additionally, there is a subcommand `make clean` to remove all previously generated data, or use `via clean` with
additional flags to remove only certain parts. Examples:

```
via clean --all # Remove generated configs, database and backups.
via clean --config # Remove configs only.
via clean --database # Remove database.
via clean --backups # Remove backups.
via clean --database --backups # Remove database *and* backups, but not configs.
```

**When do you need it?**

1. If you have an initialized database and want to run `via init`, you have to remove the database first.
2. If after getting new functionality from the `main` branch your code stopped working and `via init` doesn't help, you
   may try removing `$VIA_HOME/etc/env/target/dev.env` and running `via init` once again. This may help if the
   application configuration has changed.

If you don’t need all of the `via init` functionality, but just need to start/stop containers, use the following
commands:

```
via up   # Set up `bitcoind`, `bitcoin-cli`, `postgres` and `celestia-node` containers
via up --docker-file $VIA_HOME/docker-compose-via-btc-explorer.yml # Set up `btc-explorer-frontend`, `btc-explorer-backend` and `btc-explorer-db` containers
via down # Shut down `bitcoind`, `bitcoin-cli`, `postgres`, `celestia-node`, `btc-explorer-frontend`, `btc-explorer-backend` and `btc-explorer-db` containers
```

## Setup Verifier Network

To run the Verifier Network with the Coordinator node and one additional Verifier node for the first time, run the
following commands in a separate terminals:

Terminal 1

```
make via-coordinator
```

Terminal 2

```
make via-verifier
```

These commands will create `$VIA_HOME/etc/env/target/via_coordinator.env` and
`$VIA_HOME/etc/env/target/via_verifier.env` respectively, initialize and run the Coordinator and Verifier nodes.

In order to restart the Coordinator and Verifier without reloading the .env files run these commands:

Terminal 1

```
make via-restart-coordinator
```

Terminal 2

```
make via-restart-verifier
```

## Committing changes

`via` uses pre-commit and pre-push git hooks for basic code integrity checks. Hooks are set up automatically within the
workspace initialization process. These hooks will not allow to commit the code which does not pass several checks.

Currently the following criteria are checked:

- Rust code should always be formatted via `cargo fmt`.
- Other code should always be formatted via `zk fmt`.
- Dummy Prover should not be staged for commit (see below for the explanation).

## Using Dummy Prover

By default, the chosen prover is a "dummy" one, meaning that it doesn't actually compute proofs but rather uses mocks to
avoid expensive computations in the development environment.

To switch dummy prover to real prover, one must change `dummy_verifier` to `false` in `contracts.toml` for your env
(most likely, `etc/env/base/contracts.toml`) and run `via init` to redeploy smart contracts.

## Testing

- Running the `rust` unit-tests:

  ```
  via test rust
  ```

- Running a specific `rust` unit-test:

  ```
  via test rust --package <package_name> --lib <mod>::tests::<test_fn_name>
  # e.g. via test rust --package zksync_core --lib eth_sender::tests::resend_each_block

  via test rust -p 'via_*' # run all Via tests (from all crates prefixed with via_)
  ```

## Contracts

### Re-build contracts

```
via contract build
```

### Deploy L2 contracts

```
via contract deploy-l2
```
