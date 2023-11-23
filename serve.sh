#!/bin/bash

export BOOTSTRAP_NODE="ws://localhost:9944"
export NODE_CONTRACT="todo"
export USER_CONTRACT="todo"
export NODE_COUNT=5
export FRONTEND_PORT=7777
export RUST_LOG="info"
export RUST_BACKTRACE=1

RELEASE=""
TARGET_DIR="target/debug"
if [ "$1" = "release" ]; then
  RELEASE="--release"
  TARGET_DIR="target/release"
fi

on_exit() {
	pkill node-template
	pkill miner
	pkill runner
	pkill trunk
}

trap on_exit EXIT

(cd forked/substrate-node-template && cargo build --release || exit 1)
(cd contracts/node_staker && cargo contract build $RELEASE || exit 1)
(cd contracts/user_manager && cargo contract build $RELEASE || exit 1)
forked/substrate-node-template/target/release/node-template --dev 2>&1 > /dev/null &
sleep 1
export NODE_CONTRACT=$(cd contracts/node_staker &&\
  cargo contract instantiate --suri //Alice -x --skip-confirm --output-json | jq -r '.contract')
export USER_CONTRACT=$(cd contracts/user_manager &&\
  cargo contract instantiate --suri //Alice -x --skip-confirm --output-json | jq -r '.contract')

echo "node contract: $NODE_CONTRACT"
echo "user contract: $USER_CONTRACT"

cargo build $RELEASE --workspace \
	--exclude client \
	--exclude websocket-websys \
	--exclude node_staker \
	--exclude user_manager \
	--exclude indexed_db || exit 1
(cd nodes/client && trunk build $RELEASE --features building || exit 1)

(cd nodes/client && trunk serve $RELEASE --port $FRONTEND_PORT --features building > /dev/null &)
target/debug/runner --node-count $NODE_COUNT &

read -p "press enter to exit"

