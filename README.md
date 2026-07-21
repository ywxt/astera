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

The default is the nested winit backend. To run directly on DRM/KMS through
libseat, use:

```sh
cargo run -p astera -- --backend=native
```

The native backend requires an active seat (for example seatd or logind) and
permission to open the DRM and input devices. It discovers GPUs and connectors
through udev, assigns a CRTC and preferred mode to every usable connector, and
renders each output with its own GBM/KMS swapchain. Connector removal unbinds
the workspace without destroying it; reconnecting an output binds the first
available background workspace.

Astera prints the socket name when it starts. Launch a client from another
terminal with the printed value, for example:

```sh
WAYLAND_DISPLAY=astera-1 weston-terminal
```

The compositor also creates `<runtime-dir>/<wayland-display>.ipc`. It accepts one
RON-encoded `astera_ipc::Request` per connection (the client must close its write
half) and returns a RON-encoded `Response<DesktopSnapshot>`. Protocol v3 exposes
output focus, workspace bind/swap, window transfer, mode changes and camera
commands, plus per-output physical/logical size, scale and transform updates.
`GetState` reports both visible and background workspaces and complete output
configuration.

For a human-readable workspace overview and output status, run:

```sh
WAYLAND_DISPLAY=astera-1 cargo run -p astera --bin astera-msg -- overview
```

The active output is marked with `*`; unbound workspaces are explicitly shown as
`background`.

Default nested-backend bindings use the logo/Super modifier:

- `Super+1` through `Super+9`: show that workspace on the active output, swapping
  with the workspace already there;
- `Super+Shift+1` through `Super+Shift+9`: send the focused window;
- `Super+Space`: toggle tiled/floating;
- `Super+F`: enter fullscreen or restore the previous mode;
- `Super+Arrow`: pan the current workspace camera by 160 logical units.

Pointer behavior:

- a normal left click focuses the topmost surface and is forwarded to the client;
- `Super+Left Drag` moves tiled or floating windows without sending the drag to
  the client;
- tiled movement is previewed during the drag, snaps to a neighboring edge within
  24 logical units, then runs the radial solver once on release;
- floating movement is clamped to the current viewport; fullscreen windows cannot
  be dragged.

Camera policy is stored by the workspace and follows it between outputs.
`Centered` places the focused tiled window at the viewport center;
`KeepVisible` performs only the minimum pan required to keep the complete window
inside the configured margin. Focusing floating or fullscreen content never moves
the world camera. Workspace swaps restore focus and xdg activation on the output
that receives the workspace.

XDG popups are tracked as children of their parent, rendered and hit-tested with
the parent transform, receive frame callbacks, and support nested keyboard and
pointer grabs. The compositor also implements wlr-layer-shell: background,
bottom, top and overlay surfaces remain output-local, while fullscreen windows
sit below overlay and above top-layer content. Exclusive layer surfaces take
keyboard focus; `OnDemand` surfaces can take it on click.

Both backends advertise `wl_output`, xdg-output, viewporter and fractional scale
protocols. Only surfaces in the workspace currently owned by an output
receive output-enter and preferred-scale events; background workspaces receive
neither. This preserves the single-output ownership invariant and avoids asking
one surface to satisfy two output scales simultaneously.

Native outputs are arranged left-to-right for pointer traversal. Relative pointer
motion crosses output boundaries and changes the active output; compositor window
drags remain clamped to their source output. Per-output physical size, logical
size, fractional scale and transform can be changed atomically with the protocol
v3 `ConfigureOutput` IPC command.

Astera remains experimental. Configurable binding files and animated layout
transitions are not implemented yet.
