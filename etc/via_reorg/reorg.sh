#!/bin/bash
set -euo pipefail

# ==============================
# Config
# ==============================
SLEEP=10
SEQUENCER_NAME="via_server"
BRIDGE_ADDR="bcrt1p3s7m76wp5seprjy4gdxuxrr8pjgd47q5s8lu9vefxmp0my2p4t9qh6s8kq"
L2_RECEIVER="0x36615Cf349d7F6344891B1e7CA7C72883F5dc049"

# ==============================
# Helpers
# ==============================
pause() {
    echo "⏳ Waiting $SLEEP seconds..."
    sleep "$SLEEP"
}

manage_via_server() {
    local ACTION=$1
    local PID=$(pgrep -f "$SEQUENCER_NAME")

    case "$ACTION" in
        stop)
            if [ -n "$PID" ]; then
                echo "🛑 Stopping $SEQUENCER_NAME (PID: $PID)..."
                kill -SIGINT $PID
                sleep 2
            else
                echo "⚠️ $SEQUENCER_NAME is not running."
            fi
            ;;
        start)
            if [ -n "$PID" ]; then
                echo "⚠️ $SEQUENCER_NAME is already running (PID: $PID)."
            else
                echo "▶️ Starting $SEQUENCER_NAME..."
                make via-restart
                sleep 1
                echo "✅ $SEQUENCER_NAME started with PID: $(pgrep -f "$SEQUENCER_NAME")"
            fi
            ;;
        start-bg)
            if [ -n "$PID" ]; then
                echo "⚠️ $SEQUENCER_NAME is already running (PID: $PID)."
            else
                echo "▶️ Starting $SEQUENCER_NAME..."
                make via-restart &
                sleep 1
                echo "✅ $SEQUENCER_NAME started with PID: $(pgrep -f "$SEQUENCER_NAME")"
            fi
            ;;
        *)
            echo "Usage: manage_via_server {start|stop|restart}"
            return 1
            ;;
    esac
}

deposit() {
    local AMOUNT=$1
    local L1_RPC=$2
    local PRIV_KEY=${3:-}

    echo "💰 Depositing ${AMOUNT} BTC → $L2_RECEIVER (RPC: $L1_RPC)"
    via token deposit \
        --amount "$AMOUNT" \
        --receiver-l2-address "$L2_RECEIVER" \
        --bridge-address "$BRIDGE_ADDR" \
        ${PRIV_KEY:+--sender-private-key "$PRIV_KEY"} \
        --l1-rpc-url "$L1_RPC"

    pause
}

withdraw() {
    local AMOUNT=$1
    local RECEIVER=$2

    echo "💸 Withdrawing ${AMOUNT} BTC → $RECEIVER"
    via token withdraw \
        --amount "$AMOUNT" \
        --receiver-l1-address "$RECEIVER"

    pause
}

isolate_node() {
    local NODE=$1
    local TARGET=$2
    echo "🔌 Disconnecting $NODE from $TARGET"
    docker exec -it "$NODE" bash -c \
        "IP=\$(getent hosts $TARGET | awk '{print \$1}') && bitcoin-cli \$RPC_ARGS setban \$IP add"
}

reconnect_node() {
    local NODE=$1
    local TARGET=$2
    echo "🔄 Reconnecting $NODE to $TARGET"
    docker exec -it "$NODE" bash -c \
        "IP=\$(getent hosts $TARGET | awk '{print \$1}') && bitcoin-cli \$RPC_ARGS setban \$IP remove"
}

mine_blocks() {
    local NODE=$1
    local WALLET=$2
    local COUNT=$3
    echo "⛏️ Mining $COUNT blocks on $NODE ($WALLET)"
    docker exec -it "$NODE" bash -c \
        "ADDR=\$(bitcoin-cli \$RPC_ARGS -rpcwallet=$WALLET getnewaddress) && bitcoin-cli \$RPC_ARGS generatetoaddress $COUNT \$ADDR"
    pause
}

update_env_var() {
    local ENV_FILE="etc/env/target/via.env"
    local KEY=$1
    local VALUE=$2

    if grep -q "^$KEY=" "$ENV_FILE"; then
        echo "🔄 Updating $KEY in $ENV_FILE..."
        sed -i "s|^$KEY=.*|$KEY=$VALUE|" "$ENV_FILE"
    else
        echo "➕ Adding $KEY to $ENV_FILE..."
        echo "$KEY=$VALUE" >> "$ENV_FILE"
    fi
}

# ==============================
# Scenario
# ==============================

echo "🚀 Starting test scenario..."
# Deposits on node1
deposit 1 http://0.0.0.0:18443
deposit 2 http://0.0.0.0:18443

# Partition nodes
isolate_node via-core-bitcoin-cli2-1 bitcoind
isolate_node via-core-bitcoin-cli-1 bitcoind2
pause

# Deposits on node2 (isolated)
deposit 3 http://0.0.0.0:19443 cRaUbRSn8P8cXUcg6cMZ7oTZ1wbDjktYTsbdGw62tuqqD9ttQWMm
deposit 4 http://0.0.0.0:19443 cRaUbRSn8P8cXUcg6cMZ7oTZ1wbDjktYTsbdGw62tuqqD9ttQWMm

# Stop the server
manage_via_server stop
pause

# Update ENVs
update_env_var VIA_BTC_SENDER_BLOCK_TIME_TO_PROOF 900 
pause

manage_via_server start-bg
pause

# Clean depositor context for reorg simulation
rm -f core/lib/via_btc_client/depositor_inscriber_context.json
pause

# Deposit + withdrawal on node1
deposit 5 http://0.0.0.0:18443 cQ4UHjdsGWFMcQ8zXcaSr7m4Kxq9x7g9EKqguTaFH7fA34mZAnqW
# withdraw 1 bcrt1qx2lk0unukm80qmepjp49hwf9z6xnz0s73k9j56

# Mine conflicting blocks
mine_blocks via-core-bitcoin-cli-1 Alice 2
mine_blocks via-core-bitcoin-cli2-1 Bob 20

# Reconnect nodes
reconnect_node via-core-bitcoin-cli-1 bitcoind2
reconnect_node via-core-bitcoin-cli2-1 bitcoind

# Pause the time the nodes resync and reorg happens
pause
pause
pause

# Trigger rollback
manage_via_server stop
pause

export $(grep -v '^#' etc/env/target/via.env | xargs)
pause

make rollback to_batch=2
pause

# Update ENVs
update_env_var VIA_BTC_SENDER_BLOCK_TIME_TO_PROOF 0 
pause

manage_via_server start

echo "✅ Scenario complete."
