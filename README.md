# Orion network

## Dependencies

- rust toolchain - can be installed [here](https://www.rust-lang.org/tools/install)
- trunk - `cargo install trunk`

## Run backend

```bash
cargo build --release --workspace --exclude client --exclude websocket-websys
./target/release/runner
```

## Run Frontend

```bash
cd nodes/client
trunk serve --port 7777
```
