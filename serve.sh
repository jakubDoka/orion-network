#!/bin/bash

creq() { [ -x "$(command -v $1)" ] || cargo install $1; }

creq trunk
creq cargo-contract
creq live-server

sod() { export "$1"="${!1:-$2}"; }

sod CHAIN_NODE "ws://localhost:9944"
sod NODE_CONTRACT "todo"
sod USER_CONTRACT "todo"
sod NODE_COUNT 15
sod IDLE_TIMEOUT 2000
sod FRONTEND_PORT 7777
sod TOPOLOGY_PORT 8888
sod RUST_LOG "info"
sod RUST_BACKTRACE 1
sod NODE_START 8800
sod NETWORK_BOOT_NODE "/ip4/127.0.0.1/tcp/$((NODE_START + 100))/ws"
sod MIN_NODES 5
sod BALANCE 10000000000000
sod TEST_WALLETS 5CwfgYUrq24dTpfh2sQ2st1FNCR2fM2JFSn3EtdWyrGdEaER,5E7YrzVdg1ovRYfWLQG1bJV7FvZWJpnVnQ3nVCKEwpFzkX8s

TARGET_DIR="target/debug"
if [ "$1" = "release" ]; then
  FLAGS="--profile native-optimized"
  WASM_FLAGS="--release"
  TARGET_DIR="target/native-optimized"
fi

on_exit() { pkill node-template miner runner trunk live-server; }
trap on_exit EXIT

rm -rf node_keys node_logs
mkdir node_keys node_logs

# build
(cd nodes/client/integration && npm i || exit 1)
(cd forked/substrate-node-template && cargo build --release || exit 1)
(cd contracts/node_staker && cargo contract build $WASM_FLAGS || exit 1)
(cd contracts/user_manager && cargo contract build $WASM_FLAGS || exit 1)
cargo build $FLAGS --workspace \
	--exclude client \
	--exclude websocket-websys \
	--exclude node_staker \
	--exclude user_manager \
	--exclude topology-vis \
	--exclude indexed_db || exit 1
(cd nodes/client && trunk build $WASM_FLAGS --features building || exit 1)
(cd nodes/topology-vis && ./build.sh "$1" || exit 1)

# setup chain
forked/substrate-node-template/target/release/node-template --dev > /dev/null 2>&1 &
sleep 1
export NODE_CONTRACT=$(cd contracts/node_staker &&\
  cargo contract instantiate --suri //Alice -x --skip-confirm --output-json | jq -r '.contract')
export USER_CONTRACT=$(cd contracts/user_manager &&\
  cargo contract instantiate --suri //Alice -x --skip-confirm --output-json | jq -r '.contract')
echo "node contract: $NODE_CONTRACT"
echo "user contract: $USER_CONTRACT"
$TARGET_DIR/init-transfer || exit 1

# run
(cd nodes/topology-vis/dist && live-server --host localhost --port $TOPOLOGY_PORT &)
(cd nodes/client && trunk serve $WASM_FLAGS --port $FRONTEND_PORT --features building &)
$TARGET_DIR/runner --node-count $NODE_COUNT --first-port $NODE_START --miner $TARGET_DIR/miner &

read -p "press enter to exit"
