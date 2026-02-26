# MEV Backtest Dashboard

A browser-based dashboard for the MEV Backtest Toolkit, compiled to
WebAssembly (WASM) and deployable to GitHub Pages.

## Prerequisites

```bash
# Install the WASM target
rustup target add wasm32-unknown-unknown

# Install Trunk (static-site builder for Rust/WASM)
cargo install trunk
```

## Development

```bash
cd crates/mev-dashboard

# Serve locally with hot-reload
trunk serve          # http://127.0.0.1:8080

# Build a release bundle
trunk build --release   # output: dist/
```

## Deploy to GitHub Pages

Push to `main` — the `.github/workflows/dashboard.yml` workflow will
build and deploy automatically.

## Architecture

| Layer | File | Purpose |
|-------|------|---------|
| Types | `src/types.rs` | Shared data structures |
| RPC | `src/rpc.rs` | JSON-RPC client (browser fetch) |
| Analysis | `src/analysis.rs` | MEV detection (sandwich, arb, ordering) |
| UI | `src/components/*.rs` | Yew function components |
| Entry | `src/main.rs` | App shell and state machine |

The dashboard runs **entirely in the browser** — it talks directly to
Alchemy (or any CORS-enabled RPC) and performs analysis in WASM.
No server required.

## Note

This crate is **not** part of the Cargo workspace because it targets
`wasm32-unknown-unknown`. Build it independently with Trunk.
