# Via Network: A L2 for Bitcoin 
[![Banner](viaBanner.png)](https://onvia.org/)

Via Network is a Layer 2 scaling solution for Bitcoin that uses ZKP and DA approaches to scale the network. It is a fork of the
ZkSync project by Matter Labs. The project is currently in alpha state and is under active development.

## Knowledge Index

The following questions will be answered by the following resources:

| Question                                                | Resource                                       |
| ------------------------------------------------------- | ---------------------------------------------- |
| What do I need to develop the project locally?          | [development.md](docs/guides/development.md)   |
| How can I set up my dev environment?                    | [setup-dev.md](docs/guides/setup-dev.md)       |
| How can I run the project?                              | [launch.md](docs/guides/launch.md)             |
| What is the logical project structure and architecture? | [architecture.md](docs/guides/architecture.md) |
| Via Network Guides                                      | [via.md](docs/guides/via.md)                   |
| Where can I find protocol specs?                        | Ping Via Team Members                          |    
| Where can I find developer docs?                        | Ping Via Team Members                          |

## High Level Overview

![High Level Architecture](architecture.png)

This repository will contain code for the following components:
- Sequencer
- Proposer
  - Via bitcoin inscription manager
- Prover
- Verifier network node
  - MPC manager

`/core/bin` will contain the binaries for the above components with prefix `via_` e.g. `via_server` for sequencer and  proposer software. 

Prover related code is in the  directory `/prover`.

## Branches

- `main` is the main branch for the project. the code in this branch is the most stable and is used for production.
- `zksync-main`: this branch is equivalent to the zksync-era repo `main` branch.
- (feat/fix/chore)/`<branch-name>`: these branches are used for development and are merged into the `main` branch.
- release/`<version>`: these branches are used for release based on the `main` branch.
  
> Since we like to be updated with the latest changes in the zksync repo, we will periodically sync the `zksync-main` branch with the zksync repo and then merge the changes into the `main` branch. (rebase)

> We also adopt an approach to reduce the possibility of merge conflicts by adding a `via_` prefix to services and components that we add to the project and also creating our own new orchestration layer (binaries) for via project.

```
git remote add upstream git@github.com:matter-labs/zksync-era.git
git checkout zksync-main
git pull upstream main
git checkout main
git rebase zksync-main
```

> This approach will changing our git history, so we will need to force push to the `main` branch after the rebase.
> Please be careful when using this approach and communicate with the team before doing so.

## Disclaimer

The Via Network is under development and has not been audited. Please use the project or code of the project at your own risk.