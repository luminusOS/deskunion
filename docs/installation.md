# Installation

Prebuilt packages for Linux, Windows and macOS are on the [releases page](https://github.com/luminusOS/deskunion/releases). Release assets are named `deskunion-<version>-<platform>-<arch>`, and `SHA256SUMS.txt` lists their checksums.

To build from source, see [building.md](building.md).

## Linux

Install a Flatpak bundle or run an AppImage. Both are built for x86_64 and aarch64.

```sh
flatpak install --user deskunion-<version>-linux-x86_64.flatpak
```

For a Nix flake, see [nix/README.md](../nix/README.md).

## Windows

Run `deskunion-<version>-windows-x86_64-setup.exe`. The installer includes GTK, libadwaita, icons and schemas, and installs DeskUnion under `Program Files` with Start Menu and optional desktop shortcuts.

The portable ZIP is also available. Extract the whole `deskunion` directory and run `deskunion\bin\deskunion.exe`. Do not move the executable out of its folder, because GTK loads its icon theme and runtime data from the `bin` and `share` layout.

### Firewall

DeskUnion talks to its own daemon over a named pipe, and in client mode it only dials out, so it opens no listening port. Windows Defender may still show the "allow network access" prompt once, and dismissing it leaves a block rule behind. The ZIP ships a script that registers the program explicitly. Run it from an elevated PowerShell:

```powershell
# client machine (dials out only)
powershell -ExecutionPolicy Bypass -File windows-firewall.ps1

# server machine (also listens on UDP 4242)
powershell -ExecutionPolicy Bypass -File windows-firewall.ps1 -Server
```

### Client role

Set the role in `%LOCALAPPDATA%\deskunion\config.toml`. Without `operation_mode`, a file that still has any `[[clients]]` entry is inferred to be a server and opens the listen port.

```toml
operation_mode = "client"
server_ips = ["10.0.2.2"]
server_port = 4242
# and no [[clients]] entries
```

## macOS

1. Download the package for your Mac (Intel or ARM) from the releases page and unzip it.
2. Remove the quarantine flag: `xattr -rd com.apple.quarantine "Deskunion.app"`.
3. Launch the app and grant accessibility permissions in System Settings.

Bundled macOS builds run as a menu bar app and keep no Dock icon. Use the menu bar item to open the window or quit.

## Android

The release has `deskunion-<version>-android-<abi>.apk` for `arm64-v8a`, `x86_64` and `universal`. A separate proof of concept for Android and iOS, by [rohitsangwan01](https://github.com/rohitsangwan01), is at [deskunion-mobile](https://github.com/rohitsangwan01/deskunion-mobile). It works as a remote control for any device DeskUnion supports.

## Manual installation on Linux

Install the [build dependencies](building.md#dependencies) first, then compile (see [building.md](building.md)) or take a release binary. To add the desktop entry, icons and firewall rule:

```sh
# install deskunion (replace path/to/ with the correct path)
sudo cp path/to/deskunion /usr/local/bin/

# install app icon
sudo mkdir -p /usr/local/share/icons/hicolor/scalable/apps
sudo cp crates/deskunion-gtk/resources/io.github.luminusos.DeskUnion.svg /usr/local/share/icons/hicolor/scalable/apps

# install the symbolic app icon
sudo mkdir -p /usr/local/share/icons/hicolor/symbolic/apps
sudo cp crates/deskunion-gtk/resources/io.github.luminusos.DeskUnion-symbolic.svg /usr/local/share/icons/hicolor/symbolic/apps

# update icon cache
gtk-update-icon-cache /usr/local/share/icons/hicolor/

# install desktop entry
sudo mkdir -p /usr/local/share/applications
sudo cp io.github.luminusos.DeskUnion.desktop /usr/local/share/applications

# when using firewalld: install the firewall rule, then enable the
# service in the firewalld settings
sudo cp firewall/deskunion.xml /etc/firewalld/services
```

## Ports and firewall

The server, the machine with the physical mouse and keyboard, listens on UDP port `4242`. Each client dials out to it. Only the server needs an open firewall port, and clients work through NAT. If the server runs on the host of a GNOME Boxes VM, the guest reaches it at `10.0.2.2:4242`, the default slirp NAT gateway, with no port forwarding.
