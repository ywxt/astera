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
and IPC contracts, and Smithay winit and DRM/KMS backends. Native Wayland clients
connect through `xdg-shell`; their toplevels are placed by the radial layout
engine and rendered with GLES.

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

GitHub Actions runs formatting, Clippy, tests, a nested Xvfb smoke test, coverage and an aarch64
portability check. Nightly jobs add ASan and a five-minute layout fuzz run. Run the same
fuzzer locally with:

```sh
cargo install cargo-fuzz
cargo fuzz run layout_transactions
```

Core line coverage is enforced at 90%, portable support crates at 70%, and the
whole workspace is ratcheted from its current systems-heavy baseline toward 70%; pull requests
also require 90% coverage on changed lines.
Coverage XML and any render diagnostics are uploaded as workflow artifacts. Pixel golden files
are never accepted automatically.

The default is the nested winit backend. To run directly on DRM/KMS through
libseat, use:

```sh
cargo run -p astera -- --backend=native
```

## Configuration

Astera loads `$XDG_CONFIG_HOME/astera/config.kdl`, falling back to
`$HOME/.config/astera/config.kdl`. Pass `--config PATH` to require a specific
file. Without a file, Astera uses its built-in bindings; once a valid file
exists, its `bindings` map completely replaces the built-ins. Deleting the file
restores them.

The file is watched while Astera runs. Changes are applied after a short
debounce; an invalid edit leaves the last valid configuration active. Gap,
camera policy, keyboard repeat settings and bindings reload as one transaction.

```kdl
general {
    gap 8
}
input {
    repeat-delay 300
    repeat-rate 25
}
camera {
    keep-visible margin=32
}

bind "Super+Return" {
    spawn "kitty"
}
bind "Super+1" {
    focus-workspace 1
}
bind "Super+2" {
    focus-workspace 2 output="DP-1"
}
bind "Super+Shift+2" {
    move-window-to-workspace 2
}
bind "Super+H" {
    focus-window "left"
}
bind "Super+F" {
    toggle-fullscreen
}
bind "Super+Ctrl+F" {
    set-window-mode "fullscreen"
}
bind "Super+Right" repeat=#true {
    pan-camera 160 0
}
bind "Super+code:0x7b" {
    close-window
}
```

`spawn` takes an executable followed by literal arguments; it never invokes a
shell implicitly. Use `spawn "sh" "-c" "…"` when shell syntax is intentional.
`astrology config check`, `config format`, and `config generate` work without a
running compositor. The latter prints a complete built-in-style configuration;
add `--write` to create the default file without overwriting an existing one.

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

Both backends use one calloop-driven runtime. Wayland clients, IPC, configuration
inotify, input, session/udev, DRM page flips, exported GPU fences and timer
deadlines wake the compositor directly; idle compositors do not poll. Repaints
are coalesced per output, only one native frame may await presentation per
output, and frame callbacks are completed from the exact submitted snapshot
after host redraw, DRM vblank, or the next retrace for an unchanged scanout.
Transient frame failures use bounded deadline backoff rather than a busy retry.

Astera prints the socket name when it starts. Launch a client from another
terminal with the printed value, for example:

```sh
WAYLAND_DISPLAY=astera-1 weston-terminal
```

The compositor also creates `<runtime-dir>/astera/<wayland-display>.ipc`. The
parent directory is mode `0700`, the socket is mode `0600`, and Linux peer
credentials must report the compositor user's UID. Every frame is one
newline-terminated `<version> <RON>` record; inbound records are limited to
64 KiB and each write has a two-second timeout.

IPC has two connection modes:

- A command connection processes multiple requests strictly in order. It reads
  the next request only after writing the previous response. Query responses,
  successful mutations and errors all carry the current public `sequence`.
- Sending `EventStream` permanently upgrades that connection. Its handshake is
  one authoritative `DesktopSnapshot` at sequence `N`; subsequent records are
  `EventEnvelope`s numbered `N + 1`, `N + 2`, and so on. Commands therefore use
  another connection once the upgrade completes.

Clients first send the permanently frozen v0 `Versions` bootstrap request,
choose an overlap with the returned bounds, then use that version for the whole
connection. A mismatched later frame is an error. Wire schemas live in separate
version modules so a newer server can encode responses for supported older
clients. The current and minimum command protocol versions are both 1.

The global sequence advances once per public event, including while nobody is
subscribed. Stream registration and snapshot capture happen together on the
compositor thread, so no event can fall between the snapshot and `N + 1`.
Clients must treat a gap, malformed record, EOF or read error as loss of
synchronization: discard all cached state and reconnect for a new snapshot.
Sequences do not survive compositor restart. A stream has a bounded queue of
256 events; a lagging subscriber is disconnected rather than stalling the
compositor. Astera permits at most 64 event streams and 128 command connections.

Protocol v1 exposes output focus and configuration, workspace
focus/move/name operations, window transfer and mode changes, and camera
commands. Outputs can be selected by ID, stable key or active output.
Workspaces can be selected by global ID, unique name, or output-local index.
`GetState` reports outputs, layers, ordered dynamic workspaces, windows,
cameras, focus and config status in one snapshot. Incremental events cover
output, layer, workspace and window lifecycle/changes, workspace activation,
window/output focus, camera placement and config reload completion. A config
reload event is emitted after all state changes caused by that reload.

IPC coordinates are logical rather than physical pixels. Tiled placement uses
the workspace's infinite-world coordinates. Floating placement is local to the
complete output viewport and may overlap layer-shell exclusive zones.
Maximized windows use the output usable area; fullscreen windows use the full
viewport. `visible_geometry`, when present in a complete window snapshot, is
the full camera-projected viewport-local rectangle and is not clipped. Pure
camera/output visibility changes are represented by their own events rather
than emitting placement changes for every affected window.

For a human-readable workspace overview and output status, run:

```sh
WAYLAND_DISPLAY=astera-1 cargo run -p astera --bin astrology -- overview
```

The active output is marked with `*`; detached workspaces are shown as
`background`. `astrology` uses the current shell's `WAYLAND_DISPLAY`. A host
terminal often still points at the parent compositor, so set it to the display
name Astera prints at startup. Connection errors include the complete IPC
socket path to make a mismatched environment immediately visible.

`astrology` groups operations by resource and prints human-readable output by
default:

```sh
# Human overview or stable JSON state.
astrology state
astrology state --json

# Initial snapshot followed by events; --json emits NDJSON.
astrology events --json

# Omitted output and window arguments mean the active/focused object.
astrology output focus
astrology workspace focus 3
astrology workspace focus 3 --output DP-1
astrology workspace rename id:7 code
astrology window focus left
astrology window mode fullscreen
astrology window move code --activate
astrology camera pan 160 0

# Configuration operations are offline and need no WAYLAND_DISPLAY.
astrology config check
astrology config format --check
astrology config generate
astrology config generate --write
```

Output selectors accept a numeric ID, stable connector key, or `active`.
Workspace selectors are positional: `3` is an output-local one-based index,
`code` is a unique name, and `id:7` is a global ID. Only an index may include
`--output`; otherwise it resolves on the active output. Successful mutations
print the public sequence watermark. Exit code 2 denotes CLI/config errors, 3
IPC connection or framing failures, 4 server rejection, and 5 stream loss.

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
output. The nested backend can change per-output physical size, logical size,
fractional scale and transform atomically with protocol v1 `ConfigureOutput`.
The native backend currently rejects this command until KMS reconfiguration can
be completed atomically with its IPC response, instead of publishing metadata
that may disagree with scanout.

Astera remains experimental. Animated layout transitions are not implemented yet.
