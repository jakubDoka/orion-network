# Orion network

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
