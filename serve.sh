#!/bin/bash

export CHAIN_NODE="ws://localhost:9944"
export NODE_CONTRACT="todo"
export USER_CONTRACT="todo"
export NODE_COUNT=15
export IDLE_TIMEOUT=2000
export FRONTEND_PORT=7777
export RUST_LOG="info"
export RUST_BACKTRACE=1
export NODE_START=8800
export NETWORK_BOOT_NODE="/ip4/127.0.0.1/tcp/$((NODE_START + 100))/ws"
export MIN_NODES=5
export BALANCE=10000000000000
export TEST_WALLETS=5CwfgYUrq24dTpfh2sQ2st1FNCR2fM2JFSn3EtdWyrGdEaER,5E7YrzVdg1ovRYfWLQG1bJV7FvZWJpnVnQ3nVCKEwpFzkX8s

TARGET_DIR="target/debug"
if [ "$1" = "release" ]; then
  FLAGS="--profile native-optimized"
  WASM_FLAGS="--release"
  TARGET_DIR="target/native-optimized"
fi

on_exit() {
	pkill node-template
	pkill miner
	pkill runner
	pkill trunk
	pkill live-server
}

trap on_exit EXIT
rm -rf node_keys node_logs
mkdir node_keys node_logs

(cd forked/substrate-node-template && cargo build --release || exit 1)
(cd contracts/node_staker && cargo contract build $WASM_FLAGS || exit 1)
(cd contracts/user_manager && cargo contract build $WASM_FLAGS || exit 1)
forked/substrate-node-template/target/release/node-template --dev 2>&1 > /dev/null &
sleep 1
export NODE_CONTRACT=$(cd contracts/node_staker &&\
  cargo contract instantiate --suri //Alice -x --skip-confirm --output-json | jq -r '.contract')
export USER_CONTRACT=$(cd contracts/user_manager &&\
  cargo contract instantiate --suri //Alice -x --skip-confirm --output-json | jq -r '.contract')

echo "node contract: $NODE_CONTRACT"
echo "user contract: $USER_CONTRACT"

cargo build $FLAGS --workspace \
	--exclude client \
	--exclude websocket-websys \
	--exclude node_staker \
	--exclude user_manager \
	--exclude topology-vis \
	--exclude indexed_db || exit 1
(cd nodes/topology-vis && ./build.sh "$1" || exit 1)

$TARGET_DIR/init-transfer || exit 1
(cd nodes/topology-vis/dist && live-server --host localhost --port 8888 &)
$TARGET_DIR/runner --node-count $NODE_COUNT --first-port $NODE_START --miner $TARGET_DIR/miner &
(cd nodes/client && trunk serve $WASM_FLAGS --port $FRONTEND_PORT --features building > /dev/null &)

read -p "press enter to exit"

