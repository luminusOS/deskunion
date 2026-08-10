# Deskunion Agent Instructions

## Overview

Deskunion is an open-source Software KVM sharing mouse/keyboard input across local networks. The Rust workspace combines a GTK frontend, CLI/daemon mode, and multi-OS capture/emulation backends for Linux, Windows, and macOS.

## Core principles

- **Scope discipline.** Only implement what was requested; describe follow-up work instead of absorbing it.
- **Clarify OS behavior.** Ask when requirements touch OS-specific capture/emulation (they differ significantly).
- **Docs stay current.** Update [README.md](README.md) or [DOC.md](DOC.md) when touching public APIs or platform support.
- **Rust idioms.** Use `Result`/`Option`, `thiserror` for errors, descriptive logs, and concise comments for non-obvious invariants.

## Terminology

- **Client:** A remote machine paired with this server by certificate fingerprint. Each client is either _active_ (receiving events) or _inactive_ (can send events back). This mutual exclusion prevents feedback loops.
- **Backend:** OS-specific implementation for capture or emulation (e.g., libei, layer-shell, wlroots, X11, Windows, macOS).
- **Handle:** A per-client identifier used to route events and track state (pressed keys, position).

## Architecture

**Pipeline:** `input-capture` → `deskunion-ipc` → `input-emulation`

- **Connection direction:** All traffic is DTLS over a single UDP port (default 4242). `OperationMode::Server` (capture side, physical mouse/keyboard) is the only listener; `OperationMode::Client` (emulation side) dials out to its configured server endpoint (`server_hostname`/`server_ips`/`server_port`). Only the server needs an open firewall port — clients work through NAT. Pairing is by certificate fingerprint: the server authorizes an incoming device's fingerprint, the device is then auto-paired on the first free screen edge (`ClientManager::first_free_position`, right → left → top → bottom) and only waits "parked" for a manual assignment when all four edges are taken; the binding persists as `fingerprint` in the `[[clients]]` entry. Liveness: the listener pings (~5 s), the dialer answers `Pong(emulation_active)`; any datagram counts as alive, ~6 misses closes the connection deterministically.
- **input-capture:** Reads OS events into a `Stream<CaptureEvent>`. Backends tried in priority order (libei → layer-shell → X11 → fallback). Tracks `pressed_keys` to avoid stuck modifiers. `position_map` queues events when multiple clients share a screen edge.
- **input-emulation:** Replays events via the `Emulation` trait (`consume`, `create`, `destroy`, `terminate`). Maintains `pressed_keys` and releases them on disconnect.
- **Frontend <-> daemon IPC:** local only, never a network socket — a unix socket on Unix, a named pipe (`\\.\pipe\deskunion`) on Windows. The dialing UDP socket binds the routed interface address, not the wildcard, so a client-mode machine never looks like a server to Windows Defender (`connect.rs::local_bind_addr`).
- **deskunion-ipc / deskunion-proto:** Protocol glue and serialization. Everything (input events, control datagrams, audio) travels over the same DTLS/UDP connection. Version bumps required when serialization changes.
- **input-event:** Shared scancode enums and abstract event types—extend here, don't duplicate translations.
- **deskunion-audio:** One-directional (client → server) audio streaming, parallel to the input pipeline: the client captures its system output (WASAPI loopback, PipeWire monitor, CoreAudio loopback on macOS ≥ 14.6) and the server plays it back. cpal for capture/playback, Opus for encoding, a jitter buffer with clock-drift compensation on the receive side. The output device **pulls**: its callback pops the jitter buffer (`receiver::JitterSource`) so playback runs on the device's clock. Never reintroduce a `thread::sleep` tick that pushes into a ring — `sleep` only overshoots, the ring drains, and playback goes permanently silent while everything upstream still looks healthy. Wire format is a separate datagram type in `deskunion-proto` (not part of the `Copy` `ProtoEvent` enum), gated behind the `audio` cargo feature (default on). Controlled from the gtk frontend's Audio page via `FrontendRequest`/`FrontendEvent` variants in `deskunion-ipc`.

## Feature & cfg discipline

- Feature flags live in root `Cargo.toml`. Gate OS-specific modules with the configs exported in build.rs (e.g., `cfg(layer_shell)`).
- Prefer module-level gating over per-function cfgs to avoid empty stubs.
- New backends: add feature in `Cargo.toml`, create gated module, log backend selection.

## Async patterns

- Tokio runtime with `futures` streams and `async_trait`. Model new flows as streams or async methods.
- Avoid blocking; use `spawn_blocking` if needed. Prefer existing single-threaded stream handling.
- `InputCapture` implements `Stream` and manually pumps backends—don't short-circuit this logic.

## Commands

```sh
cargo build --workspace                                    # full build
cargo build -p <crate>                                     # single crate
cargo test --workspace                                     # all tests
cargo fmt && cargo clippy --workspace --all-targets --all-features  # lint
RUST_LOG=deskunion=debug cargo run                         # debug logging
```

Run from repo root—no `cd` in scripts.

## Testing

- Unit tests for utilities; integration tests for protocol behavior.
- OS-specific backends: test via GTK/CLI on target OS or document manual verification.
- Dummy backend exercises pipeline without real dependencies.
- Verify `terminate()` releases keys on unexpected disconnect.

## Workflow

1. Clarify ambiguous requirements, especially OS-specific behavior.
2. Implement minimal change; flag follow-up work.
3. Add proportional tests; run `cargo test` on affected crates.
4. Run `cargo fmt` and `cargo clippy --workspace --all-targets --all-features`.
