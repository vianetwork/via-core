# Colors
YELLOW = \033[0;33m
RESET = \033[0m

# CLI tool
CLI_TOOL = via


CMD := $(firstword $(MAKECMDGOALS))
VIA_ENV ?= via
DIFF ?= 0
MODE ?= sequencer

# Select the via env
ifeq ($(CMD), via-verifier)
    VIA_ENV := via_verifier
	DIFF := 1
	MODE := verifier
else ifeq ($(CMD), via-restart)
    VIA_ENV := via
else ifeq ($(CMD), via-restart-verifier)
	VIA_ENV := via_verifier
else ifeq ($(CMD), via-coordinator)
	VIA_ENV := via_coordinator
	DIFF := 2
	MODE := coordinator
else ifeq ($(CMD), via-restart-coordinator)
	VIA_ENV := via_coordinator
else ifeq ($(CMD), via-indexer)
	VIA_ENV := via_indexer
	DIFF := 3
	MODE := indexer
else ifeq ($(CMD), via-restart-indexer)
	VIA_ENV := via_indexer
endif

# Default target: Show help message
.PHONY: help
help:
	@echo "------------------------------------------------------------------------------------"
	@echo "$(YELLOW)VIA Protocol Makefile$(RESET)"
	@echo "------------------------------------------------------------------------------------"
	@echo ""
	@echo "Usage: make [target]"
	@echo ""
	@echo "Available Targets:"
	@echo "  via                - Run the basic VIA setup workflow (without Bitcoin explorer)."
	@echo "  all                - Run the full VIA setup workflow (with Bitcoin explorer)."
	@echo "  env                - Set the correct environment for VIA protocol."
	@echo "  config             - Create the basic configuration files for VIA."
	@echo "  init               - Initialize the project by pulling Docker images and running migrations."
	@echo "  transactions       - Send random transactions on the Bitcoin regtest network."
	@echo "  celestia           - Update Celestia config and obtain test tokens."
	@echo "  btc-explorer       - Run a Bitcoin explorer."
	@echo "  bootstrap-dev      -- Send the bootstrapping inscription to Bitcoin (Development)."
	@echo "  server-genesis     - Populate the genesis block data if no genesis block exists."
	@echo "  server             - Run the sequencer software."
	@echo "  clean              - Clean the project, remove Docker images, volumes, and generated files."
	@echo "  rollback           - Initiate a sequencer rollback by specifying the target 'to_batch' number."
	@echo "  help               - Show this help message (default target)."
	@echo ""
	@echo "Requirements:"
	@echo "  - Set up the VIA_HOME and PATH environment variables."
	@echo "  - Compile the 'via' command for the first time."
	@echo "  - Install Docker."
	@echo "------------------------------------------------------------------------------------"

# Default target: Redirect to help
.DEFAULT_GOAL := help

# Restart the sequence
.PHONY: via-restart
via-restart: env-soft server

# Run the basic setup workflow in sequence
.PHONY: via
via: base transactions celestia bootstrap-dev server-genesis server

# Run the full setup workflow in sequence
.PHONY: all
all: base transactions celestia btc-explorer bootstrap-dev server-genesis server

# Run the basic setup workflow in verifier
.PHONY: via-verifier
via-verifier: base celestia verifier

# Restart the verifier
.PHONY: via-restart-verifier
via-restart-verifier: env-soft verifier

# Run the basic setup workflow for the coordinator
.PHONY: via-coordinator
via-coordinator: base celestia verifier

# Restart the coordinator
.PHONY: via-restart-coordinator
via-restart-coordinator: env-soft verifier

# Run the L1 indexer
.PHONY: via-indexer
via-indexer: base l1-indexer

# Restart the coordinator
.PHONY: via-restart-indexer
via-restart-indexer: env-soft l1-indexer

# Run minimal required setup
.PHONY: base
base: env config init

# Run 'via env via'
.PHONY: env
env:
	@echo "------------------------------------------------------------------------------------"
	@echo "$(YELLOW)Setting the environment...$(RESET)"
	@echo "------------------------------------------------------------------------------------"
	@$(CLI_TOOL) env ${VIA_ENV}

# Run 'via env via --soft'
.PHONY: env-soft
env-soft:
	@echo "------------------------------------------------------------------------------------"
	@echo "$(YELLOW)Setting the environment...$(RESET)"
	@echo "------------------------------------------------------------------------------------"
	@$(CLI_TOOL) env ${VIA_ENV} --soft

# Run 'via config compile'
.PHONY: config
config:
	@echo "------------------------------------------------------------------------------------"
	@echo "$(YELLOW)Creating environment configuration file...$(RESET)"
	@echo "------------------------------------------------------------------------------------"
	@$(CLI_TOOL) config compile ${VIA_ENV} ${DIFF}

# Run 'via init'
.PHONY: init
init:
	@echo "------------------------------------------------------------------------------------"
	@echo "$(YELLOW)Initializing the project...$(RESET)"
	@echo "------------------------------------------------------------------------------------"
	@$(CLI_TOOL) init --mode ${MODE}

# Run 'via transactions'
.PHONY: transactions
transactions:
	@echo "------------------------------------------------------------------------------------"
	@echo "$(YELLOW)Sending random transactions on the regtest network...$(RESET)"
	@echo "------------------------------------------------------------------------------------"
	@$(CLI_TOOL) transactions

# Run 'via celestia'
.PHONY: celestia
celestia:
	@echo "------------------------------------------------------------------------------------"
	@echo "$(YELLOW)Updating Celestia configuration and obtaining TIA test tokens...$(RESET)"
	@echo "------------------------------------------------------------------------------------"
	@$(CLI_TOOL) celestia

# Run 'via btc-explorer'
.PHONY: btc-explorer
btc-explorer:
	@echo "------------------------------------------------------------------------------------"
	@echo "$(YELLOW)Running a Bitcoin explorer...$(RESET)"
	@echo "------------------------------------------------------------------------------------"
	@$(CLI_TOOL) btc-explorer

# Run 'via bootstrap-dev'
.PHONY: bootstrap-dev
bootstrap-dev:
	@echo "------------------------------------------------------------------------------------"
	@echo "$(YELLOW)Sending bootstrapping inscription to Bitcoin...$(RESET)"
	@echo "------------------------------------------------------------------------------------"
	@$(CLI_TOOL) bootstrap system-bootstrapping \
		--private-key cVZduZu265sWeAqFYygoDEE1FZ7wV9rpW5qdqjRkUehjaUMWLT1R \
		--start-block 1 \
		--verifiers-pub-keys 03d8e2443ef58aa80fb6256bf3b94d2ecf9117f19cb17661ec60ad35fd84ff4a8b,02043f839b8ecd9ffd79f26ec7d05750555cd0d1e0777cfc84a29b7e38e6324662 \
		--governance-address bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56 \
		--bridge-address bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq \
		--sequencer-address bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56

	@echo "$(YELLOW)Sending attestations...$(RESET)"
	@$(CLI_TOOL) bootstrap attest-sequencer-proposal --private-key cRaUbRSn8P8cXUcg6cMZ7oTZ1wbDjktYTsbdGw62tuqqD9ttQWMm
	@$(CLI_TOOL) bootstrap attest-sequencer-proposal --private-key cQ4UHjdsGWFMcQ8zXcaSr7m4Kxq9x7g9EKqguTaFH7fA34mZAnqW

	@echo "$(YELLOW)Update ENVs...$(RESET)"
	@$(CLI_TOOL) bootstrap update-bootstrap-tx

# Run 'via server --genesis'
.PHONY: server-genesis
server-genesis:
	@echo "------------------------------------------------------------------------------------"
	@echo "$(YELLOW)Populating data for the genesis block...$(RESET)"
	@echo "------------------------------------------------------------------------------------"
	@$(CLI_TOOL) server --genesis

# Run 'via server'
.PHONY: server
server:
	@echo "------------------------------------------------------------------------------------"
	@echo "$(YELLOW)Running the sequencer software...$(RESET)"
	@echo "------------------------------------------------------------------------------------"
	@$(CLI_TOOL) server

# Run 'via verifier'
.PHONY: verifier
verifier:
	@echo "------------------------------------------------------------------------------------"
	@echo "$(YELLOW)Running the verifier/coordinator software...$(RESET)"
	@echo "------------------------------------------------------------------------------------"
	@$(CLI_TOOL) verifier

# Run 'via clean'
.PHONY: clean
clean:
	@echo "------------------------------------------------------------------------------------"
	@echo "$(YELLOW)Cleaning the project, removing images, volumes, and generated files...$(RESET)"
	@echo "------------------------------------------------------------------------------------"
	@$(CLI_TOOL) clean

# Run 'via l1-indexer'
.PHONY: l1-indexer
l1-indexer:
	@echo "------------------------------------------------------------------------------------"
	@echo "$(YELLOW)Running the L1 indexer software...$(RESET)"
	@echo "------------------------------------------------------------------------------------"
	@$(CLI_TOOL) indexer

# Require 'to_batch' args as input, ex: `make rollback to_batch=2`
.PHONY: rollback
rollback:
	cargo run --bin via_block_reverter_cli -- rollback-db \
		--rollback-postgres \
		--l1-batch-number $(to_batch) \
		--rollback-tree \
		--rollback-sk-cache \
		--rollback-vm-runners-cache \
		--rollback-snapshots