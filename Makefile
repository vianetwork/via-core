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
else ifeq ($(CMD), via-coordinator)
	VIA_ENV := via_coordinator
	DIFF := 2
	MODE := coordinator
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
	@echo "  bootstrap          - Send the bootstrapping inscription to Bitcoin."
	@echo "  server-genesis     - Populate the genesis block data if no genesis block exists."
	@echo "  server             - Run the sequencer software."
	@echo "  clean              - Clean the project, remove Docker images, volumes, and generated files."
	@echo "  help               - Show this help message (default target)."
	@echo ""
	@echo "Requirements:"
	@echo "  - Set up the VIA_HOME and PATH environment variables."
	@echo "  - Compile the 'via' command for the first time."
	@echo "  - Install Docker."
	@echo "------------------------------------------------------------------------------------"

# Default target: Redirect to help
.DEFAULT_GOAL := help

# Run the basic setup workflow in sequence
.PHONY: via
via: base transactions celestia bootstrap server-genesis server

# Run the full setup workflow in sequence
.PHONY: all
all: base transactions celestia btc-explorer bootstrap server-genesis server

# Run the basic setup workflow in verifier
.PHONY: via-verifier
via-verifier: base celestia verifier

# Run the basic setup workflow in coordinator
.PHONY: via-coordinator
via-coordinator: base celestia verifier

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

# Run 'via bootstrap'
.PHONY: bootstrap
bootstrap:
	@echo "------------------------------------------------------------------------------------"
	@echo "$(YELLOW)Sending bootstrapping inscription to Bitcoin...$(RESET)"
	@echo "------------------------------------------------------------------------------------"
	@$(CLI_TOOL) bootstrap

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
