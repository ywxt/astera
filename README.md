# Astera

Astera is an experimental infinite-canvas Wayland compositor written in Rust.
Windows live in workspace coordinates while outputs act as viewports into that
world. Opening or placing a tiled window pushes intersecting tiled windows
outward until the layout is stable.

The repository currently contains the compositor-independent layout engine,
configuration and IPC contracts, and the compositor process skeleton. Smithay
backend and rendering integration are the next implementation milestone.

## Build

```sh
cargo test --workspace
cargo run -p astera
```

