<p align="center">
  <img src="crates/deskunion-gtk/resources/io.github.luminusos.DeskUnion.svg"
       alt="Deskunion logo"
       width="240">
</p>

<h1 align="center">Deskunion</h1>

[![CI](https://github.com/luminusOS/deskunion/actions/workflows/rust.yml/badge.svg)](https://github.com/luminusOS/deskunion/actions/workflows/rust.yml) [![Cachix](https://github.com/luminusOS/deskunion/actions/workflows/cachix.yml/badge.svg)](https://github.com/luminusOS/deskunion/actions/workflows/cachix.yml) [![Release](https://github.com/luminusOS/deskunion/actions/workflows/release.yml/badge.svg)](https://github.com/luminusOS/deskunion/actions/workflows/release.yml)

[![crates.io](https://img.shields.io/crates/v/deskunion-app.svg)](https://crates.io/crates/deskunion-app)  [![license](https://img.shields.io/crates/l/deskunion-app.svg)](https://github.com/luminusOS/deskunion/blob/main/Cargo.toml)

Deskunion is a *cross-platform* mouse and keyboard sharing software similar to universal-control on Apple devices.
It allows for using multiple PCs via a single set of mouse and keyboard.
This is also known as a Software KVM switch.

Goal of this project is to be an open-source alternative to proprietary tools like [Synergy 2/3](https://symless.com/synergy), [Share Mouse](https://www.sharemouse.com/de/)
and other open source tools like [Deskflow](https://github.com/deskflow/deskflow) or [Input Leap](https://github.com/input-leap) (Synergy fork).

Focus lies on performance, ease of use and a maintainable implementation that can be expanded to support additional backends for e.g. Android, iOS, ... in the future.

***blazingly fast™*** because it's written in rust.

- _Now with a gtk frontend_
- _Now with audio streaming (client → server)_

<picture>
    <source media="(prefers-color-scheme: dark)" srcset="/screenshots/dark.png?raw=true">
    <source media="(prefers-color-scheme: light)" srcset="/screenshots/light.png?raw=true">
    <img alt="Screenshot of Deskunion" srcset="/screenshots/dark.png">
</picture>


## Encryption

Deskunion encrypts all network traffic using the DTLS implementation provided by [WebRTC.rs](https://github.com/webrtc-rs/webrtc).
There are currently no mitigations in place for timing side-channel attacks.

## Audio Streaming

Deskunion can stream a client machine's audio to the server it's connected
to, so audio plays out of the server's speakers alongside the shared mouse
and keyboard. Streaming is one-directional (client → server) and opt-in on
both ends — sending and receiving are toggled independently.

- Codec: Opus, over the same DTLS-encrypted UDP channel used for input
  events.
- Capture/playback: [cpal](https://github.com/RustAudio/cpal), which covers
  PipeWire loopback capture on Linux, WASAPI loopback on Windows, and
  CoreAudio on macOS 14.6+ (older macOS versions can still capture a
  microphone, just not system output).
- A jitter buffer with clock-drift compensation absorbs network jitter and
  slowly-diverging sender/receiver clocks.
- Controlled from the gtk frontend's **Audio** page: enable send/receive,
  pick capture/playback devices, adjust bitrate and jitter buffer size, and
  watch active streams' latency/loss/level.
- Built via the `audio` cargo feature (enabled by default); disable it with
  `--no-default-features` if you don't need it — see
  [Conditional compilation](#conditional-compilation) below.

## OS Support

Most current desktop environments and operating systems are fully supported, this includes
- GNOME >= 45
- KDE Plasma >= 6.1
- Most wlroots based compositors, including Sway (>= 1.8), Hyprland and Wayfire
- Windows
- MacOS


### Caveats / Known Issues

> [!Important]
> - **X11** currently only has support for input emulation, i.e. can only be used on the receiving end.
>
> - **Sway / wlroots**: Wlroots based compositors without libei support on the receiving end currently do not handle modifier events on the client side.
> This results in CTRL / SHIFT / ALT / SUPER keys not working with a sending device that is NOT using the `layer-shell` backend
>
> - **Wayfire**: If you are using [Wayfire](https://github.com/WayfireWM/wayfire), make sure to use a recent version (must be newer than October 23rd) and **add `shortcuts-inhibit` to the list of plugins in your wayfire config!**
> Otherwise input capture will not work.
>
> - **Windows**: The mouse cursor will be invisible when sending input to a Windows system if
> there is no real mouse connected to the machine.

For more detailed information about os support see [Detailed OS Support](#detailed-os-support)

### Android & IOS

A proof of concept for an Android / IOS Application by [rohitsangwan01](https://github.com/rohitsangwan01) can be found [here](https://github.com/rohitsangwan01/deskunion-mobile).
It can be used as a remote control for any device supported by Deskunion.

## Installation

<details>
    <summary>Arch Linux</summary>

Deskunion can be installed from the [official repositories](https://archlinux.org/packages/extra/x86_64/deskunion/):

```sh
pacman -S deskunion
```

The prerelease version (following `main`) is available on the AUR:

```sh
paru -S deskunion-git
```
</details>


<details>
    <summary>Nix (OS)</summary>

- nixpkgs: [search.nixos.org](https://search.nixos.org/packages?channel=unstable&show=deskunion&from=0&size=50&sort=relevance&type=packages&query=deskunion)
- flake: [README.md](./nix/README.md)
</details>

<details>
    <summary>Fedora</summary>
You can install Deskunion from the [Terra Repository](https://terra.fyralabs.com).


After enabling Terra:

```sh
dnf install deskunion
```
</details>

<details>
    <summary>MacOS</summary>

- Download the package for your Mac (Intel or ARM) from the releases page
- Unzip it
- Remove the quarantine with `xattr -rd com.apple.quarantine "Deskunion.app"`
- Launch the app
- Use the menu bar item to open the settings window or quit Deskunion. Bundled macOS builds run as a menu bar app and do not keep a Dock icon visible.
- Grant accessibility permissions in System Preferences

</details>


<details>
    <summary>Manual Installation</summary>

First make sure to [install the necessary dependencies](#installing-dependencies-for-development--compiling-from-source).

Precompiled release binaries for Windows, MacOS and Linux are available in the [releases section](https://github.com/luminusOS/deskunion/releases).
For Windows, the depenedencies are included in the .zip file, for other operating systems see [Installing Dependencies](#installing-dependencies-for-development--compiling-from-source).

Alternatively, the `deskunion` binary can be compiled from source (see below).

### Installing desktop file, app icon and firewall rules (optional)
```sh
# install deskunion (replace path/to/ with the correct path)
sudo cp path/to/deskunion /usr/local/bin/

# install app icon
sudo mkdir -p /usr/local/share/icons/hicolor/scalable/apps
sudo cp crates/deskunion-gtk/resources/io.github.luminusos.DeskUnion.svg /usr/local/share/icons/hicolor/scalable/apps

# update icon cache
gtk-update-icon-cache /usr/local/share/icons/hicolor/

# install desktop entry
sudo mkdir -p /usr/local/share/applications
sudo cp io.github.luminusos.DeskUnion.desktop /usr/local/share/applications

# when using firewalld: install firewall rule
sudo cp firewall/deskunion.xml /etc/firewalld/services
# -> enable the service in firewalld settings
```

Instead of downloading from the releases, the `deskunion` binary
can be easily compiled via cargo or nix:

### Compiling and installing manually:
```sh
# compile in release mode
cargo build --release

# equivalent explicit package selection
cargo run -p deskunion-app

# build Linux packages, including the AppImage
cargo install cargo-bundle
cargo bundle --release
# output: target/release/bundle/appimage/deskunion_*.AppImage

# install deskunion
sudo cp target/release/deskunion /usr/local/bin/
```

Connection direction: the **server** (the machine with the physical mouse and
keyboard) listens on UDP port `4242`; each **client** (a machine that emulates
the received input) dials out to the server. Only the server needs an open
firewall port — clients need none and work through NAT. If the server runs on
the host of a GNOME Boxes VM, the guest reaches it at `10.0.2.2:4242` (the
default slirp NAT gateway), with no port forwarding required.

### Compiling and installing via cargo:
```sh
# will end up in ~/.cargo/bin
cargo install deskunion-app
```

### Compiling and installing via nix:
```sh
# you can find the executable in result/bin/deskunion
nix-build
```
### Conditional compilation
Support for other platforms is omitted automatically based on the active
rust toolchain.

Additionally, available backends and frontends can be configured manually via
[cargo features](https://doc.rust-lang.org/cargo/reference/features.html).

E.g. if only support for sway is needed, the following command produces
an executable with support for only the `layer-shell` capture backend
and `wlroots` emulation backend:
```sh
cargo build --no-default-features --features layer_shell_capture,wlroots_emulation
```
For a detailed list of available features, checkout the [Cargo.toml](./Cargo.toml)
</details>



## Development

### Git pre-commit hook

This repository includes a local git hooks directory `.githooks/` with a `pre-commit` script that enforces formatting, lints, and tests before allowing a commit.  It is optional to enable it, but it will prevent you from committing code with failing unit tests or that needs clippy/fmt fixes. To enable the hook locally:

1. Make the hook executable:

```sh
chmod +x .githooks/pre-commit
```

2. Point git to the hooks directory (one-time per clone):

```sh
git config core.hooksPath .githooks
```

The `pre-commit` script runs `cargo fmt --all` (and fails if files were modified), `cargo clippy --workspace --all-targets --all-features -- -D warnings`, and `cargo test --workspace --all-features`.

### Dependencies & Compiling from Source
<details>
    <summary>MacOS</summary>

```sh
# Install dependencies
brew install libadwaita pkg-config imagemagick
cargo install cargo-bundle
# Create the macOS icon file
scripts/makeicns.sh
# Create the .app bundle
cargo bundle
# Copy all dynamic libraries into the bundle, and update the bundle to find them there
scripts/copy-macos-dylib.sh
```
</details>

<details>
    <summary>Ubuntu and derivatives</summary>

```sh
sudo apt install libadwaita-1-dev libgtk-4-dev libx11-dev libxtst-dev
```
</details>

<details>
    <summary>Arch and derivatives</summary>

```sh
sudo pacman -S libadwaita gtk libx11 libxtst
```
</details>

<details>
    <summary>Fedora and derivatives</summary>

```sh
sudo dnf install libadwaita-devel libXtst-devel libX11-devel
```
</details>
<details>
    <summary>Nix</summary>

```sh
nix-shell .
```
</details>
<details>
    <summary>Nix (flake)</summary>

```sh
nix develop
```
</details>

<details>
    <summary>Windows</summary>

The release ZIP is self-contained. Extract the complete `deskunion` directory
and run `deskunion\bin\deskunion.exe`; do not copy the executable away from its
`bin`, `share`, and `lib` directories, because GTK loads its icon theme and
runtime data from that layout.

DeskUnion talks to its own daemon over a named pipe and, in client mode, only
dials out — it opens no listening port. Windows Defender may still show the
"allow network access" prompt once, and dismissing it leaves a *block* rule
behind. The ZIP ships a script that registers the program explicitly; run it
from an elevated PowerShell:

```powershell
# client machine (dials out only)
powershell -ExecutionPolicy Bypass -File windows-firewall.ps1

# server machine (also listens on UDP 4242)
powershell -ExecutionPolicy Bypass -File windows-firewall.ps1 -Server
```

Set the role explicitly in `%LOCALAPPDATA%\deskunion\config.toml`. Without
`operation_mode`, a file that still carries any `[[clients]]` entry is inferred
to be a **server** and opens the listen port:

```toml
operation_mode = "client"
server_ips = ["10.0.2.2"]
server_port = 4242
# and no [[clients]] entries
```

- First install [Rust](https://www.rust-lang.org/tools/install).

- Then follow the instructions at [gtk-rs.org](https://gtk-rs.org/gtk4-rs/stable/latest/book/installation_windows.html)

*TLDR:*

Build gtk from source

- The following commands should be run in an **admin power shell** instance:
```sh
# install chocolatey
Set-ExecutionPolicy Bypass -Scope Process -Force; iex ((New-Object System.Net.WebClient).DownloadString('https://community.chocolatey.org/install.ps1'))

# install gvsbuild dependencies
choco install python git msys2 visualstudio2022-workload-vctools
```

- The following commands should be run in a **regular power shell** instance:

```sh
# install gvsbuild with python
python -m pip install --user pipx
python -m pipx ensurepath
```

- Relaunch your powershell instance so the changes in the environment are reflected.
```sh
pipx install gvsbuild

# build gtk + libadwaita
gvsbuild build gtk4 libadwaita librsvg adwaita-icon-theme
```

- **Make sure to add the directory** `C:\gtk-build\gtk\x64\release\bin`
[**to the `PATH` environment variable**]((https://learn.microsoft.com/en-us/previous-versions/office/developer/sharepoint-2010/ee537574(v=office.14))). Otherwise the project will fail to build.

To avoid building GTK from source, it is possible to disable
the gtk frontend (see conditional compilation).
</details>

## Usage
<details>
    <summary>Gtk Frontend</summary>

By default the gtk frontend will open when running `deskunion`.

The machine whose mouse and keyboard you want to share runs in **server** mode
(`Operation mode` selector in the sidebar): it listens for incoming connections
on UDP port `4242` (configurable). Each machine you want to control runs in
**client** mode: enter the server's hostname or IP address and port in the
**Server** section and click `Connect`. The client only dials out, so it works
behind NAT (e.g. a GNOME Boxes VM connecting to its host at `10.0.2.2:4242`)
and needs no firewall changes.

When a client connects for the first time, an authorization dialog pops up on
the **server** showing the client's certificate fingerprint (also visible on
the client under the general section, of the form "aa:bb:cc:..."). Authorize
it — the device is then paired automatically and placed on the first free
screen edge, preferring the right one (right, then left, top, bottom). This
persists the fingerprint binding in the configuration file (see
[Configuration](#configuration)); you can move the device to another edge at
any time from the Screens page.

Only when all four edges are already taken does the device show up under
"Devices awaiting a position" on the server's Screens page and wait for you to
pick an edge by hand.

If the device still can not be entered, make sure UDP port `4242`
(or the selected port) is open in the **server's** firewall. The client opens
no ports at all. On Windows use `windows-firewall.ps1` from the release ZIP
(`-Server` on the server machine) — it also clears the block rule a dismissed
Defender prompt leaves behind.
</details>

<details>
    <summary>Command Line Interface</summary>

The cli interface can be accessed by passing `cli` as a commandline argument.
Use
```sh
deskunion cli help
```
 to list the available commands and
```sh
deskunion cli <cmd> help
```
for information on how to use a specific command.

</details>

<details>
    <summary>Daemon Mode</summary>

Deskunion can be launched in daemon mode to keep it running in the background (e.g. for use in a systemd-service).

To do so, use the `daemon` subcommand:

```sh
deskunion daemon
```
</details>

## Systemd Service

In order to start deskunion with a graphical session automatically,
the [systemd-service](service/deskunion.service) can be used:

Copy the file to `~/.config/systemd/user/` and enable the service:

```sh
cp service/deskunion.service ~/.config/systemd/user
systemctl --user daemon-reload
systemctl --user enable --now deskunion.service
```
> [!Important]
> Make sure to point `ExecStart=/usr/bin/deskunion daemon` to the actual `deskunion` binary (in case it is not under `/usr/bin`, e.g. when installed manually.


## Configuration
To automatically load clients on startup, the file `$XDG_CONFIG_HOME/deskunion/config.toml` is parsed.
`$XDG_CONFIG_HOME` defaults to `~/.config/`.

The GTK sidebar exposes an **Operation mode** selector:

- `server` captures this computer's keyboard and pointer, listens for incoming
  client connections on UDP port `4242` and sends input to paired clients;
- `client` dials out to a configured server, accepts remote input and emulates
  it on this computer.

DeskUnion starts only the backend required by the selected mode, so opening
the application does not request unrelated input permissions. The choice is
stored in `config.toml`. Fresh installations request no input permission until
a mode is selected; legacy configurations containing clients are inferred as
`server`, and configurations with `server_hostname` set are inferred as
`client`.

To create this file you can copy the following example config:

### Example config
> [!TIP]
> key symbols in the release bind are named according
> to their names in [crates/input-event/src/scancode.rs#L172](crates/input-event/src/scancode.rs#L176).
> This is bound to change

```toml
# example configuration

# operation role (server | client). When absent it is inferred: a file
# with [[clients]] entries is a server, one with server_hostname/server_ips
# is a client, and an empty one is unconfigured. Set it explicitly —
# a leftover [[clients]] entry otherwise turns a client into a server
# and opens the listen port.
operation_mode = "server"

# configure release bind
release_bind = [ "KeyA", "KeyS", "KeyD", "KeyF" ]

# optional port the server listens on (defaults to 4242)
port = 4242

# client mode only: the server this device dials out to
# (operation_mode = "client"); the client opens no ports itself
# server_hostname = "my-server.local"
# server_ips = ["192.168.178.10"]
# server_port = 4242

# list of authorized tls certificate fingerprints that
# are accepted for incoming traffic
[authorized_fingerprints]
"bc:05:ab:7a:a4:de:88:8c:2f:92:ac:bc:b8:49:b8:24:0d:44:b3:e6:a4:ef:d7:0b:6c:69:6d:77:53:0b:14:80" = "iridium"

# define a paired client on the right side with label "iridium"
[[clients]]
# position (left | right | top | bottom)
position = "right"
# display label for the device
hostname = "iridium"
# sha256 certificate fingerprint of the paired device — the stable
# identity a connecting device is matched by (assigned when pairing)
fingerprint = "bc:05:ab:7a:a4:de:88:8c:2f:92:ac:bc:b8:49:b8:24:0d:44:b3:e6:a4:ef:d7:0b:6c:69:6d:77:53:0b:14:80"
# activate this client immediately when deskunion is started
activate_on_startup = true

# define a paired client on the left side with label "thorium"
[[clients]]
position = "left"
hostname = "thorium"
fingerprint = "aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff:00:11:22:33:44:55:66:77:88:99"
```

Where `left` can be either `left`, `right`, `top` or `bottom`.

> [!NOTE]
> `ips` and `port` inside `[[clients]]` are deprecated: the server no longer
> dials out to clients — devices connect in and are matched by `fingerprint`.
> Both keys are still parsed for compatibility (and logged as deprecated) but
> are ignored; `hostname` is now only a display label.

## Roadmap
- [x] Graphical frontend (gtk + libadwaita)
- [x] respect xdg-config-home for config file location.
- [x] IP Address switching
- [x] Liveness tracking Automatically ungrab mouse when client unreachable
- [x] Liveness tracking: Automatically release keys, when server offline
- [x] MacOS KeyCode Translation
- [x] Libei Input Capture
- [x] MacOS Input Capture
- [x] Windows Input Capture
- [x] Encryption
- [ ] X11 Input Capture
- [ ] Latency measurement and visualization
- [ ] Bandwidth usage measurement and visualization
- [ ] Clipboard support


## Detailed OS Support

In order to use a device for sending events, an **input-capture** backend is required, while receiving events requires
a supported **input-emulation** *and* **input-capture** backend.

A suitable backend is chosen automatically based on the active desktop environment / compositor.

The following sections detail the emulation and capture backends provided by deskunion and their support in desktop environments / operating systems.

### Input Emulation Support

| Desktop / Backend         | wlroots                  | libei                    | remote-desktop portal    | windows                  |   macos                                | x11                |
|---------------------------|--------------------------|--------------------------|--------------------------|--------------------------|----------------------------------------|--------------------|
| Wayland (wlroots)         | :heavy_check_mark:       |                          |                          |                          |                                        |                    |
| Wayland (KDE)             |                          | :heavy_check_mark:       | :heavy_check_mark:       |                          |                                        |                    |
| Wayland (Gnome)           |                          | :heavy_check_mark:       | :heavy_check_mark:       |                          |                                        |                    |
| Windows                   |                          |                          |                          | :heavy_check_mark:       |                                        |                    |
| MacOS                     |                          |                          |                          |                          |   :heavy_check_mark:                   |                    |
| X11                       |                          |                          |                          |                          |                                        | :heavy_check_mark: |

- `wlroots`: This backend makes use of the [wlr-virtual-pointer-unstable-v1](https://wayland.app/protocols/wlr-virtual-pointer-unstable-v1) and [virtual-keyboard-unstable-v1](https://wayland.app/protocols/virtual-keyboard-unstable-v1) protocols and is supported by most wlroots based compositors.
- `libei`: This backend uses [libei](https://gitlab.freedesktop.org/libinput/libei) and is supported by GNOME >= 45 or KDE Plasma >= 6.1.
- `xdp`: This backend uses the [freedesktop remote-desktop-portal](https://flatpak.github.io/xdg-desktop-portal/#gdbus-org.freedesktop.portal.RemoteDesktop) and is supported on GNOME and Plasma.
- `x11`: Backend for X11 sessions.
- `windows`: Backend for Windows.
- `macos`: Backend for MacOS.



### Input Capture Support

| Desktop / Backend         | layer-shell              | libei                    | windows                  |   macos                                | x11 |
|---------------------------|--------------------------|--------------------------|--------------------------|----------------------------------------|-----|
| Wayland (wlroots)         | :heavy_check_mark:       |                          |                          |                                        |     |
| Wayland (KDE)             | :heavy_check_mark:       | :heavy_check_mark:       |                          |                                        |     |
| Wayland (Gnome)           |                          | :heavy_check_mark:       |                          |                                        |     |
| Windows                   |                          |                          | :heavy_check_mark:       |                                        |     |
| MacOS                     |                          |                          |                          |   :heavy_check_mark:                   |     |
| X11                       |                          |                          |                          |                                        | WIP |

- `layer-shell`: This backend creates a single pixel wide window on the edges of Displays to capture the cursor using the [layer-shell protocol](https://wayland.app/protocols/wlr-layer-shell-unstable-v1).
- `libei`: This backend uses [libei](https://gitlab.freedesktop.org/libinput/libei) and is supported by GNOME >= 45 or KDE Plasma >= 6.1.
- `windows`: Backend for input capture on Windows.
- `macos`: Backend for input capture on MacOS.
- `x11`: TODO (not yet supported)
