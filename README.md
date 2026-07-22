# Astera

Astera is an experimental infinite-canvas Wayland compositor written in Rust.
Each output owns an independent ordered set of dynamic workspaces. Every
workspace is an infinite world; opening or placing a tiled window pushes
intersecting tiled windows outward until the layout is stable.

Windows have three exclusive modes:

- tiled windows use world coordinates and participate in radial layout;
- floating windows use viewport-local coordinates and follow their workspace;
- one fullscreen window may cover the output currently showing the workspace.

Every connected output always has one empty workspace at the end. Using or
naming it creates the next placeholder. Empty, unnamed, inactive workspaces are
removed automatically. Workspace IDs are globally stable for IPC, while numbers
shown to users are one-based and local to each output.

Workspaces can move between outputs. The first connected output is primary; when
another output disconnects, its non-empty or named workspaces are appended to the
primary output. They remember their original output and return when it
reconnects. If every output disconnects, they remain detached until one returns.

The repository contains the compositor-independent layout engine, configuration
and IPC contracts, plus a first Smithay nested backend. Native Wayland clients
can connect through `xdg-shell`; their toplevels are placed by the radial layout
engine and rendered with GLES in the nested window.

## Build

```sh
cargo test --workspace
cargo run -p astera
```

## Testing

`cargo test --workspace --all-targets` runs deterministic core, configuration, IPC and
compositor-state tests. The layout solver also uses reproducible `proptest` cases; a failure
prints a seed that must be retained when turning it into a regression test. Key repeat and
animation-facing state use an injectable monotonic clock, so tests do not sleep.

GitHub Actions runs formatting, Clippy, tests, a nested Xvfb smoke test, coverage, Miri and an
aarch64 portability check. Nightly jobs add ASan and a five-minute layout fuzz run. Run the same
fuzzer locally with:

```sh
cargo install cargo-fuzz
cargo fuzz run layout_transactions
```

Core line coverage is currently ratcheted from its measured 77% baseline toward 90%, and the
whole workspace from 51% toward 70%; pull requests already require 90% coverage on changed lines.
Coverage XML and any render diagnostics are uploaded as workflow artifacts. Pixel golden files
are never accepted automatically.

The default is the nested winit backend. To run directly on DRM/KMS through
libseat, use:

```sh
cargo run -p astera -- --backend=native
```

## Configuration

Astera loads `$XDG_CONFIG_HOME/astera/config.ron`, falling back to
`$HOME/.config/astera/config.ron`. Pass `--config PATH` to require a specific
file. Without a file, Astera uses its built-in bindings; once a valid file
exists, its `bindings` map completely replaces the built-ins. Deleting the file
restores them.

The file is watched while Astera runs. Changes are applied after a short
debounce; an invalid edit leaves the last valid configuration active. Gap,
camera policy, animation duration, keyboard repeat settings and bindings reload
as one transaction.

```ron
(
    gap: 8,
    animation_ms: 280,
    camera: KeepVisible(margin: 32),
    key_repeat: (delay_ms: 300, rate: 25),
    bindings: {
        "Super+Return": Spawn(["foot"]),
        "Super+P": SpawnShell("grim -g \"$(slurp)\" | wl-copy"),
        "Super+1": FocusWorkspace(workspace: Index(1)),
        "Super+2": FocusWorkspace(workspace: Index(2, "DP-1")),
        "Super+Shift+2": MoveWindowToWorkspace(workspace: Index(2)),
        "Super+H": FocusDirection(Left),
        "Super+F": ToggleFullscreen,
        "Super+Ctrl+F": SetWindowMode(Fullscreen),
        "Super+Right": (
            action: PanCamera(x: 160, y: 0),
            repeat: true,
        ),
        "Super+code:0x7b": CloseWindow,
    },
)
```

Bindings use case-insensitive XKB keysyms or Linux evdev codes written as
`code:123`/`code:0x7b`. Modifiers are exactly `Ctrl`, `Alt`, `Shift` and `Super`;
matching is exact and ignores lock modifiers. A physical-code binding takes
priority over a keysym binding. Normalized duplicates and repeat on unsafe
actions are configuration errors.

The native backend requires an active seat (for example seatd or logind) and
permission to open the DRM and input devices. It discovers GPUs and connectors
through udev, assigns a CRTC and preferred mode to every usable connector, and
renders each output with its own GBM/KMS swapchain. Connector removal preserves
workspace layout, focus and camera state. Reconnecting the same stable output
restores its workspaces and last active workspace.

Astera prints the socket name when it starts. Launch a client from another
terminal with the printed value, for example:

```sh
WAYLAND_DISPLAY=astera-1 weston-terminal
```

The compositor also creates `<runtime-dir>/<wayland-display>.ipc`. It accepts one
RON-encoded `astera_ipc::Request` per connection (the client must close its write
half) and returns a RON-encoded `Response<DesktopSnapshot>`. Protocol v4 exposes
output focus, workspace focus/move/name operations, window transfer, mode and
camera commands, plus per-output physical/logical size, scale and transform
updates. Outputs can be selected by ID, stable key or active output. Workspaces
can be selected by global ID, unique name, or output-local index. `GetState`
reports each output's ordered workspace set, active workspace and detached
workspaces.

For a human-readable workspace overview and output status, run:

```sh
WAYLAND_DISPLAY=astera-1 cargo run -p astera --bin astrology -- overview
```

The active output is marked with `*`; detached workspaces are shown as
`background`.

When no configuration file exists, built-in bindings use the Super modifier:

- `Super+1` through `Super+9`: focus that output-local workspace index;
- `Super+Shift+1` through `Super+Shift+9`: send the focused window to that
  output-local workspace;
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
the world camera. Camera zoom is not persistent workspace state; a future
overview may apply a temporary render-only scale.

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
drags remain clamped to their source output. Floating windows keep an exact
placement cache per stable output key and a normalized fallback anchor for a new
output. Per-output physical size, logical size, fractional scale and transform
can be changed atomically with the protocol v4 `ConfigureOutput` IPC command.

Astera remains experimental. Configurable binding files and animated layout
transitions are not implemented yet.
