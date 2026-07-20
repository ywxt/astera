# Astera

Astera is an experimental infinite-canvas Wayland compositor written in Rust.
Each workspace is an independent infinite world and can be bound to at most one
output at a time. Opening or placing a tiled window pushes intersecting tiled
windows outward until the layout is stable.

Windows have three exclusive modes:

- tiled windows use world coordinates and participate in radial layout;
- floating windows use viewport-local coordinates and follow their workspace;
- one fullscreen window may cover the output currently showing the workspace.

Workspaces can move or swap between outputs. Disconnecting an output leaves its
workspace intact in the background, including camera, focus, floating and
fullscreen state.

The repository contains the compositor-independent layout engine, configuration
and IPC contracts, plus a first Smithay nested backend. Native Wayland clients
can connect through `xdg-shell`; their toplevels are placed by the radial layout
engine and rendered with GLES in the nested window.

## Build

```sh
cargo test --workspace
cargo run -p astera
```

Astera prints the socket name when it starts. Launch a client from another
terminal with the printed value, for example:

```sh
WAYLAND_DISPLAY=astera-1 weston-terminal
```

The compositor also creates `<runtime-dir>/<wayland-display>.ipc`. It accepts one
RON-encoded `astera_ipc::Request` per connection (the client must close its write
half) and returns a RON-encoded `Response<DesktopSnapshot>`. Protocol v2 exposes
output focus, workspace bind/swap, window transfer, mode changes and camera
commands. `GetState` reports both visible and background workspaces.

Default nested-backend bindings use the logo/Super modifier:

- `Super+1` through `Super+9`: show that workspace on the active output, swapping
  with the workspace already there;
- `Super+Shift+1` through `Super+Shift+9`: send the focused window;
- `Super+Space`: toggle tiled/floating;
- `Super+F`: enter fullscreen or restore the previous mode;
- `Super+Arrow`: pan the current workspace camera by 160 logical units.

This backend is an integration milestone rather than a complete desktop: pointer
focus, configurable binding files, animation, popups, layer shell and DRM/KMS are
still to be implemented.
