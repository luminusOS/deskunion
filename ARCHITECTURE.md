# DeskUnion Architecture

DeskUnion is an open-source software KVM: one keyboard and mouse control several
computers on a local network. The computer that owns the physical devices is the
**server** (it captures input); every computer it controls is a **client** (it
emulates input). Linux, Windows, macOS and Android are supported, with a GTK4 +
Libadwaita frontend, a command line interface and a background service.

## System Context

```mermaid
flowchart LR
    subgraph Server[Server computer]
        Devices[Keyboard and mouse] --> Capture[input-capture<br/>OS backend]
        Capture --> ServerSvc[deskunion service<br/>CaptureTask + DTLS listener]
        ServerUi[GTK frontend / CLI] <-->|local IPC| ServerSvc
    end
    subgraph Client[Client computer]
        ClientSvc[deskunion service<br/>EmulationTask + DTLS dialer] --> Emulation[input-emulation<br/>OS backend]
        Emulation --> ClientOs[Pointer and keyboard of the client]
        ClientUi[GTK frontend / CLI] <-->|local IPC| ClientSvc
    end
    ClientSvc -->|dials out, DTLS over UDP 4242| ServerSvc
    ServerSvc -.->|input, Enter/Leave, Ping, clipboard| ClientSvc
    ClientSvc -.->|Ack, Pong, Hello, audio, clipboard| ServerSvc
```

Connections always go from client to server. Only the server listens (UDP, port
4242 by default), so only the server needs an open firewall port; clients work
behind NAT. The same DTLS association carries input events, control datagrams,
clipboard text and audio.

## Process Model

```mermaid
flowchart TD
    Exe[deskunion executable] -->|no subcommand| Gtk[GTK frontend process]
    Gtk -->|spawns itself with daemon| Daemon[service process]
    Exe -->|daemon| Daemon
    Exe -->|cli| Cli[deskunion-cli]
    Gtk <-->|FrontendRequest / FrontendEvent| Ipc[(unix socket<br/>or named pipe)]
    Cli <--> Ipc
    Ipc <--> Daemon
    Daemon -->|single-threaded tokio + LocalSet| Tasks[Capture / Emulation / DNS tasks]
```

- Without a subcommand the executable starts the service as a child process and
  runs the GTK frontend; closing the frontend stops the child (SIGINT first on
  Unix). On Android, where the application executes as a shared library (`.so`)
  inside an APK sandbox, the service runs on a dedicated background thread in the
  same process rather than a spawned child. `deskunion daemon` runs only the service,
  which `service/deskunion.service` uses for a systemd user unit.
- The frontend never touches the network or the input devices. It talks to the
  service over a local-only channel: a unix socket on Unix and the named pipe
  `\\.\pipe\deskunion` on Windows. The `deskunion-cli` crate speaks the same
  protocol.
- The service runs on one thread (`tokio` current-thread runtime and a
  `LocalSet`), so its state uses `Rc`/`RefCell` rather than locks.

## Workspace Layout

```mermaid
flowchart TD
    Workspace[Cargo workspace] --> App[crates/deskunion-app<br/>service, config, DTLS]
    Workspace --> Gtk[crates/deskunion-gtk<br/>frontend]
    Workspace --> Cli[crates/deskunion-cli]
    Workspace --> Ipc[crates/deskunion-ipc<br/>frontend protocol]
    Workspace --> Proto[crates/deskunion-proto<br/>wire format]
    Workspace --> Audio[crates/deskunion-audio]
    Workspace --> Cap[crates/input-capture]
    Workspace --> Emu[crates/input-emulation]
    Workspace --> Ev[crates/input-event<br/>shared event types]

    App --> Ipc
    App --> Proto
    App --> Cap
    App --> Emu
    App --> Audio
    App --> Gtk
    Gtk --> Ipc
    Cli --> Ipc
    Cap --> Ev
    Emu --> Ev
    Proto --> Ev
```

## Main Crates

### `crates/deskunion-app`

The service and the `deskunion` binary. It owns configuration, pairing, the DTLS
transport and the coordination between capture, emulation and the frontends.

- `service.rs`: the `Service` event loop. Receives `FrontendRequest`s, capture and
  emulation events, DNS results, and answers with `FrontendEvent`s.
- `capture.rs`: `CaptureTask` on the server side. Owns the input-capture
  session, the DTLS listener events, the barriers of each client and the state
  machine of the active client (`WaitingForAck` then `Sending`).
- `listen.rs`: `DeskunionListener`, the DTLS listener. Accepts devices, runs the
  liveness pinger and the read loop of each connection, decodes datagrams into
  `ListenEvent`s.
- `emulation.rs` and `connect.rs`: the client side. `DeskunionConnection` dials the
  server, `server_loop` handles one established session, `connect_loop` retries.
- `retry.rs`: the reconnection schedule and the notice shown to the user.
- `client.rs`: `ClientManager`, the shared list of configured clients and their
  state (position, `active`, `active_addr`, `alive`, `ping_ms`, fingerprint).
- `config.rs`: `config.toml`, command line arguments, backend selection.
- `crypto.rs`: certificate generation and fingerprints. `dns.rs`: asynchronous
  resolution of client host names. `clipboard.rs`: text clipboard reassembly.
  `audio.rs`: glue to `deskunion-audio`.

### `crates/input-capture`

Turns the physical keyboard and mouse into a `Stream` of capture events and
grabs the pointer at screen edges. `InputCapture` wraps one backend and keeps
`id_map` and `position_map`, so several clients can share an edge. Backends are
tried in priority order when none is configured:

| Platform | Backends, in order |
|---|---|
| Linux | `input-capture-portal` (libei), `layer-shell`, `X11` |
| Windows | low-level mouse and keyboard hooks |
| macOS | event tap |
| tests | `dummy` |

`cursor_geometry.rs` holds the platform-independent geometry used by the Windows
cursor parking and is unit tested everywhere.

### `crates/input-emulation`

Replays events on the client through the `Emulation` trait (`consume`, `create`,
`destroy`, `terminate`). It tracks pressed keys and releases them when the
connection ends.

| Platform | Backends, in order |
|---|---|
| Linux | `wlroots`, `libei`, `xdg-desktop-portal` (RemoteDesktop), `X11` |
| Windows | `SendInput` |
| macOS | CoreGraphics events |
| tests | `dummy` |

### `crates/input-event`

Abstract pointer and keyboard events and the Linux scancode enums. New key
translations go here rather than in a backend.

### `crates/deskunion-proto`

The wire format. `ProtoEvent` covers `Input`, `Enter(position)`, `Leave(serial)`,
`Ack`, `Ping`, `Pong(emulation_active)` and `Hello{commit}`. Separate datagram
kinds carry the computer name (event id 17, 1 to 255 bytes of UTF-8), clipboard
fragments and acknowledgements, and audio frames and controls. Peers skip
datagram types they do not know, which keeps older versions compatible. A change
in the serialization needs a version bump.

### `crates/deskunion-ipc`

The local protocol between a frontend and the service: `FrontendRequest`
(`Activate`, `UpdatePosition`, `SetServiceRunning`, `ReconnectNow`, ...),
`FrontendEvent` (`State`, `ServerRetry`, `OperationMode`, `AudioStream`, ...) and
the serializable `ClientConfig` / `ClientState`. `connect.rs`, `connect_async.rs`
and `listen.rs` implement the sockets and pipes on both ends.

### `crates/deskunion-audio`

One-directional audio from client to server, parallel to the input pipeline. The
client captures its system output (WASAPI loopback on Windows, a PipeWire monitor
on Linux, CoreAudio loopback on macOS 14.6 or newer), encodes it with Opus and
sends it; the server decodes and plays it through a jitter buffer with
clock-drift compensation. The output device pulls from the buffer, so playback
follows the device clock. Behind the `audio` cargo feature (on by default).

### `crates/deskunion-gtk`

The Rust/Relm4 frontend, four pages: Connections, Audio, Logs and Settings.

- `app/mod.rs`: the window. An `AdwNavigationSplitView` that collapses below
  600sp, so on a phone the sidebar fills the window and pages open over it; the
  dialogs and toasts the pages share; the tray.
- `app/session.rs`: `Session`, the sharing state every page reads (role, port,
  capture and emulation, the user's Start/Stop intent, the server link, client
  summaries) and the status line derived from it; pure and unit tested.
- `app/pages/`: one Relm4 component per page (`connections`, `audio`, `logs`,
  `settings`), each owning its widgets and the state only it shows.
- `screen_arrangement/`: the drag-and-drop canvas where screens are placed on the
  four edges.
- `factories/`: list rows (clients, parked devices, incoming devices, audio
  streams, fingerprints). `dialogs/`: add client, authorization, fingerprint.
- `server_link.rs`, `ping.rs`: user-facing text and levels for the retry notice and
  for the round-trip signal icon; pure and unit tested.
- `tray/`: tray icon and menu. `tray/menu.rs` builds the menu model for every
  platform; `linux.rs` (StatusNotifierItem through ksni) and `win32.rs`
  (`tray-icon`) only render it. `background.rs` talks to the Background Apps
  portal in a Flatpak. macOS uses a menu bar item (`macos_status_item.rs`).

### `crates/deskunion-cli`

Command line front end over the same IPC, for scripting and headless machines.

## Pairing

```mermaid
sequenceDiagram
    participant C as Client
    participant S as Server
    C->>S: DTLS handshake (self-signed certificates)
    Note over S: fingerprint of the client certificate
    alt fingerprint not authorized
        S-->>C: rejected (user can authorize it on the server)
    else authorized
        S->>S: Accept
        alt fingerprint bound to a [[clients]] entry
            S->>S: client becomes connected (active_addr)
        else unknown device
            S->>S: park the device until it gets a screen edge
        end
    end
```

The stable identity of a device is its certificate fingerprint; source addresses
are only used to route datagrams. An authorized device that is not bound yet is
placed automatically on the first free edge (right, left, top, bottom) and waits
parked only when all four are taken. The binding is persisted as `fingerprint`
in the `[[clients]]` entry of `config.toml`. Only bound devices can be entered.

The client checks the server the same way, by trust on first use: it accepts any
certificate on the first connection and pins its fingerprint as
`server_fingerprint` in `config.toml`. From then on `connect.rs::verify_server`
refuses a server with another certificate. The dial reports
`ServerFingerprintMismatch` with both fingerprints, the frontend asks the user,
and `TrustServerFingerprint` pins the new one. Saving another endpoint clears the
pin. Changing only the pin keeps a session whose server already shows it, so
pinning on first use does not reconnect. The service pins only when the
`Connected` event belongs to the endpoint still configured. It keeps the last
mismatch and replays it to every frontend that syncs. A `server_fingerprint`
that is not a valid fingerprint fails closed: no server matches it. Endpoints
are compared without regard to IP order or host-name case. A connection test
uses the pin only when it tests the saved endpoint. On a dial to several
addresses, a mismatch is reported over a pending authorization, and that over
any other failure.

A client announces its computer name right after connecting and again every two
seconds. `ClientManager::set_announced_name` fills only an empty label of a bound
client and never overwrites a name the user typed.

## Input Handoff

```mermaid
sequenceDiagram
    participant P as Physical pointer
    participant S as Server (CaptureTask)
    participant C as Client (emulation)
    P->>S: touches the barrier of an edge
    S->>S: Begin: client becomes active, state WaitingForAck
    S->>C: Enter(opposite edge)
    C-->>S: Ack
    S->>S: state Sending
    loop while the pointer is captured
        P->>S: motion, keys, buttons
        S->>C: Input
    end
    C-->>S: Leave (pointer reached the far edge)
    S->>S: release the capture, send key-ups for held keys
    S-->>P: pointer returns to the server screen
```

While waiting for the `Ack`, the server repeats `Enter` instead of input, so a
lost datagram does not leave the client without the handoff. If a send fails
(the client is gone, or its emulation is off) the handoff ends at once: the
capture is released and the active client is forgotten, without the key-ups and
`Leave` that would fail the same way, so the next crossing starts with a fresh
`Enter`/`Ack`. For 300 ms after any release (`HANDOVER_GRACE`) every barrier hit
is ignored, including one from the client just left, because the returning
pointer rests on the edge it just crossed. A release bind
(`Ctrl+Shift+Alt+Meta` by default) returns the pointer to the server at any time;
before the `Leave`, the key-ups of every held key are synthesized so the peer
does not keep phantom modifiers.

### Capture barriers

A barrier at a screen edge makes the backend grab the pointer when it touches
it. `CaptureTask::sync_barriers` keeps the barriers of the backend equal to the
captures that want one: an `EnterOnly` capture (used to detect an incoming
handoff) always, a `Default` capture only while its client has an `active_addr`.
It runs after every listener event and every request, and a new capture session
starts from an empty set. A computer that is offline therefore never grabs the
pointer at its edge, on Linux through the portal or on Windows through the hook.
When the client that held the pointer loses its barrier, the pointer is released
first.

## Session Recovery

A compositor can end a portal session on its own, for example GNOME closing the
RemoteDesktop session on screen lock, or the EIS socket being reset. Both sides
start their session again without the user pressing Start:

- **Emulation (client).** `EmulationTask::run` restarts a failed session after
  250 ms, 500 ms, 1 s, 2 s, 5 s, 10 s and 30 s. Only a session that was usable for
  at least 10 s starts the delays over; time spent waiting on the portal does not
  count, so a portal that keeps failing slowly still runs out of restarts. A
  restart reuses the backend of the session that worked, never falling back to
  another one (the dummy backend would accept input and drop it). While a restart
  is pending it reports `EmulationEvent::EmulationRestarting`: the client sends
  `Leave` if it held the pointer, and the service keeps the server target, so the
  DTLS session survives. `EmulationDisabled`, which drops the target, is only sent
  once a session had been announced and then the delays run out, the user denied
  the portal prompt (`cancelled_by_user`) or turned emulation off. A disable,
  terminate or peer removal that arrives while a session is still being set up is
  applied, not lost. libei transport errors such as `ConnectionReset` or
  `BrokenPipe` count as the end of the stream; with libei that end is only noticed
  by the next event sent, so the restart usually happens on the first input after
  an unlock.
- **A compositor that stops reading.** Neither backend drops input when its socket
  is full, since a dropped key-up leaves a key stuck. libei keeps the unsent bytes
  in its write buffer and a flush task sends them once the socket is writable; if
  a flush does not complete within 5 s the session fails with `LibeiStalled`, and
  every later call fails too. wlroots reads the compositor's replies before each
  send (unread replies would block the compositor in turn) and waits for the
  socket, failing with `WaylandOutputBlocked` after 500 ms. Either error restarts
  the session as above; on wlroots that happens on the next event, since `create`
  and `destroy` only log it, and tearing down a stalled session can take about
  1 s per client.
- **Capture (server).** On any session error `do_capture` releases a held pointer
  (key-ups and `Leave`). When the desktop closed the session (`ActivationClosed`,
  `EndOfStream`, or a libei `Disconnected`), `CaptureTask::run_loop` starts it again
  after 1, 2, 5, 10, 30 s and then every 60 s, while it keeps serving listener
  events and requests. Once recovering, a failed re-creation is retried too,
  because the portal refuses new sessions while the screen is locked; a denied
  permission is not. A session that gets running, or disabling or enabling
  capture by hand, ends the recovery.

The two sides give up differently on purpose: emulation stops after about 49 s
of failed restarts and then waits for the user, because a client that keeps
failing should show as disabled; capture keeps trying every 60 s, because the
server has nothing else to do until its desktop is unlocked.

## Liveness and Link Quality

The listener is the pinger. Immediately after a device connects, and then every
5 seconds, it sends `Ping`; the client answers `Pong(emulation_active)`. Any
received datagram counts as proof of life, so a busy audio stream cannot trip the
keepalive; six consecutive probes without any datagram close the connection.

The listener also times each `Ping` against the `Pong` that answers it
(`Liveness::probed_at`) and reports `ListenEvent::Latency`, which is stored in
`ClientState::ping_ms` and shown in the client row as a signal icon and a value
in milliseconds. The icon steps are 20, 50 and 120 ms. The client has a mirrored
watchdog that closes the session when the server goes silent.

## Client Connection Retry

```mermaid
flowchart TD
    Start["set_server / reconnect_now / session dropped"] --> Resolve[resolve the host name<br/>merge with manual addresses]
    Resolve --> Dial[dial all addresses in parallel]
    Dial -->|connected| Session[server_loop<br/>backoff starts over]
    Dial -->|failed| Same{target still<br/>the same?}
    Same -->|no| Start
    Same -->|yes| Delay[retry_delay by failures<br/>15 s if authorization is pending]
    Delay --> Notice["ConnectionEvent::Retrying<br/>then FrontendEvent::ServerRetry"]
    Notice --> Wait["wait, or wake on<br/>reconnect_now / new target"]
    Wait --> Resolve
```

The schedule follows Deskflow's `ClientApp::retryTime`: 1 second for the first 300
consecutive failures, then 5, 10, 30, 60 and 120 seconds, and 5 minutes from the
430th. The host name is resolved before every attempt, so a network that comes up
late or a server that moves is picked up without restarting. A wake-up only
reaches a loop that is already waiting, so after a failed dial the loop checks
that its target is still current before announcing or sleeping. Waiting for the
server to authorize this computer uses a flat 15 seconds without backing off,
because older peers reopen their dialog on every rejected handshake.

## Frontend State

```mermaid
flowchart LR
    Svc[Service] -->|FrontendEvent| Reader[blocking reader thread]
    Reader --> Chan[async channel]
    Chan --> App[AppModel::update]
    App -->|Session::apply| Session
    App -->|Session, FrontendEvent| Pages[Connections, Audio, Logs, Settings]
    Pages -->|PageOutput| App
    App -->|FrontendRequest| Svc
    App --> Tray["tray::publish(TrayInfo)"]
    Tray --> Render[ksni / tray-icon]
    Render -->|TrayCommand| App
```

The window owns the `Session`. Each `FrontendEvent` is applied to it first and
then forwarded to every page, so a page handling an event already holds the
session that event leads to; the window pushes a new `Session` to the pages
only when it changed. Pages never talk to the service or to each other: they
send `PageOutput` (a request, a toast, a role change, Start/Stop, a dialog to
open, or the client list for the tray) and the window acts on it. After every
update `post_view` publishes a `TrayInfo` (role, whether sharing runs, status
line, clients) to the tray, which redraws only when it changed; tray actions
come back as `AppMsg::Tray` and reuse the same requests.

## Windows Specifics

- Capture uses low-level hooks (`WH_MOUSE_LL`, `WH_KEYBOARD_LL`) on a dedicated
  thread with its own message loop; emulation uses `SendInput`. Both are part of
  the software KVM behavior and are what endpoint-security products notice.
- While another computer is controlled, the cursor is parked at the center of the
  display it left from and the motion is measured from there. On return it is
  placed at the height it left, two pixels inside the crossed edge.
  `DESKUNION_PARK_CURSOR=0` turns the parking off.
- The capture thread declares per-monitor DPI awareness. The hook reports
  physical pixels and the display rectangles come from `EnumDisplaySettingsW`,
  also physical; `SetCursorPos` follows the DPI awareness of its thread and would
  otherwise scale its arguments, parking the cursor away from the center whenever
  the display scale is above 100%.
- Audio capture normally uses process loopback of every process except
  DeskUnion, which does not depend on the master volume. The Teams desktop client
  is silent in that stream, so `call_watch.rs` finds the outputs it plays on.
  Windows notifications (a session was created on an output, a Teams session
  changed state, an output was added or removed) only wake the watcher; it then
  recomputes the answer from the system, so a lost notification cannot leave it
  wrong. Because the session list may not show a new session at once, a wake-up
  is followed by quick looks for 2 s, and a look every 5 s covers anything never
  delivered. While an output has an active Teams session it is captured through
  endpoint loopback and the process loopback stops forwarding, since the output
  already holds every sound; `CallTracker` keeps the output for 3 s after the
  session goes quiet, and releasing an output immediately flushes its mixer slot
  (`clear_source`) so stale or starved samples do not leak into future calls. Loopback
  reads and mixer backlog trims strictly preserve stereo frame alignment (8 bytes per
  frame) to prevent channel swapping. The decision logic (`calls.rs`) and the mixer are unit
  tested; the WASAPI calls are only compiled and linted for Windows.
- Logs go to `%LOCALAPPDATA%\deskunion\deskunion.log`.

## Android Specifics

- The Android application runs inside the GTK Android runtime (`RuntimeApplication` and
  `ToplevelActivity`), built with GTK Android Builder (pixiewood) and Meson.
- The binary targets `android_exe_type: 'application'`, compiling the application into a
  shared library (`libdeskunion.so`) that exports `pub extern "C" fn main`.
- The background service runs in a dedicated thread inside the process rather than a spawned child,
  and local IPC communicates over Unix sockets located in `$TMPDIR` / app cache storage.
- Desktop-specific features (X11, Wayland, libei, desktop system tray) are gated off on Android,
  using the Dummy capture/emulation backends until native Android accessibility/input services are configured.

## Configuration and State

`config.toml` stores the operation mode, the capture and emulation backends
(when not automatic), the listen port, the certificate path, the release bind,
the server endpoint a client dials (`server_hostname`, `server_ips`,
`server_port`), the `authorized_fingerprints` of the server, the `[audio]` and
`[clipboard]` sections and the `[[clients]]` list with their position, port, host
name and fingerprint. Every field is optional, so older files keep loading. A
fresh installation has no operation mode and requests no permission until the
user chooses Server or Client. Each instance has exactly one
mode. In client mode the capture backend can still start later, only to detect
the handoff back to the server.

## Build and Release

- Features of `deskunion-app` select the backends (`layer_shell_capture`,
  `x11_capture`, `libei_capture`, `wlroots_emulation`, `libei_emulation`,
  `rdp_emulation`, `x11_emulation`) and the optional parts (`gtk`, `audio`).
  Gate OS-specific code at module level.
- Pushing a `v<version>` tag runs `release.yml`, which calls one reusable
  workflow per platform: a Windows installer and portable ZIP, a macOS DMG
  (`aarch64`), an Android APK (`release-android.yml`), and a Flatpak and an AppImage for `x86_64` and `aarch64`, plus
  `SHA256SUMS.txt`. The tag must match the `deskunion-app` version.
- `rust.yml` runs the tests on every platform; the graphical tests run on Linux
  under a private D-Bus session and Xvfb with the Cairo renderer.
- Packaging inputs live in `build-aux/` (Flatpak manifest, Inno Setup, Android Pixiewood manifest), `data/`
  (icons), `service/` (systemd user unit), `nix/` and `scripts/`.

## Testing

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace

# Graphical flow with a fake service; no real input devices or peers.
cargo test -p deskunion-gtk --features ui-tests -- --test-threads=1

# Optional PNG captures into an existing directory.
DESKUNION_UI_CAPTURE_DIR=/tmp/captures \
  cargo test -p deskunion-gtk --features ui-tests app::ui_tests -- --test-threads=1
```

Without a desktop, run the graphical test on a headless Wayland compositor
(`mutter --headless --wayland --no-x11 --virtual-monitor 1400x1000
--wayland-display=wayland-hl`, then `WAYLAND_DISPLAY=wayland-hl GDK_BACKEND=wayland`
inside `dbus-run-session`). Captures taken there keep the rounded window
corners; Xvfb draws them square and defaults to a 640 × 480 screen.

The `dummy` backends exercise the pipeline without real devices, and
`capture_test` / `emulation_test` run a backend by hand (`deskunion test-capture`,
`deskunion test-emulation`). Backends that need a real desktop (portals,
layer-shell, the Windows hooks and tray, macOS) are verified by running the
application on that platform.

## Not Verified

The behavior of the Windows cursor parking and tray, the macOS build and a real
suspend and resume cannot be exercised from CI. The GPU renderer, the declared
minimum library versions (GTK 4.14, libadwaita 1.5), the GNOME 50 Flatpak runtime
and screen-reader navigation have not been tested.
