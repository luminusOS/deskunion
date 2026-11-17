# OS support

Most current desktop environments and operating systems work:

- GNOME 45 and later
- KDE Plasma 6.1 and later
- Most wlroots compositors, including Sway 1.8 and later, Hyprland and Wayfire
- Windows
- macOS

A device that sends input needs an input-capture backend. A device that receives input needs an input-emulation backend and an input-capture backend. DeskUnion picks the backend from the active desktop environment or compositor.

## Known issues

> [!IMPORTANT]
> - X11 only supports input emulation, so it can only be the receiving end.
> - Sway and other wlroots compositors without libei on the receiving end do not handle modifier events. Ctrl, Shift, Alt and Super stop working when the sending device does not use the `layer-shell` backend.
> - Wayfire must be newer than October 23rd, and `shortcuts-inhibit` must be in the plugins list of the Wayfire config. Otherwise input capture does not work.
> - On Windows, the cursor is invisible when sending input to a machine that has no real mouse connected.

The tables show which backends each desktop or operating system uses.

## Input emulation

| Desktop / Backend         | wlroots                  | libei                    | remote-desktop portal    | windows                  |   macos                                | x11                |
|---------------------------|--------------------------|--------------------------|--------------------------|--------------------------|----------------------------------------|--------------------|
| Wayland (wlroots)         | ✅       |                          |                          |                          |                                        |                    |
| Wayland (KDE)             |                          | ✅       | ✅       |                          |                                        |                    |
| Wayland (Gnome)           |                          | ✅       | ✅       |                          |                                        |                    |
| Windows                   |                          |                          |                          | ✅       |                                        |                    |
| MacOS                     |                          |                          |                          |                          |   ✅                   |                    |
| X11                       |                          |                          |                          |                          |                                        | ✅ |

- `wlroots`: This backend makes use of the [wlr-virtual-pointer-unstable-v1](https://wayland.app/protocols/wlr-virtual-pointer-unstable-v1) and [virtual-keyboard-unstable-v1](https://wayland.app/protocols/virtual-keyboard-unstable-v1) protocols and is supported by most wlroots based compositors.
- `libei`: This backend uses [libei](https://gitlab.freedesktop.org/libinput/libei) and is supported by GNOME >= 45 or KDE Plasma >= 6.1.
- `xdp`: This backend uses the [freedesktop remote-desktop-portal](https://flatpak.github.io/xdg-desktop-portal/#gdbus-org.freedesktop.portal.RemoteDesktop) and is supported on GNOME and Plasma.
- `x11`: Backend for X11 sessions.
- `windows`: Backend for Windows.
- `macos`: Backend for MacOS.



## Input capture

| Desktop / Backend         | layer-shell              | libei                    | windows                  |   macos                                | x11 |
|---------------------------|--------------------------|--------------------------|--------------------------|----------------------------------------|-----|
| Wayland (wlroots)         | ✅       |                          |                          |                                        |     |
| Wayland (KDE)             | ✅       | ✅       |                          |                                        |     |
| Wayland (Gnome)           |                          | ✅       |                          |                                        |     |
| Windows                   |                          |                          | ✅       |                                        |     |
| MacOS                     |                          |                          |                          |   ✅                   |     |
| X11                       |                          |                          |                          |                                        | WIP |

- `layer-shell`: This backend creates a single pixel wide window on the edges of Displays to capture the cursor using the [layer-shell protocol](https://wayland.app/protocols/wlr-layer-shell-unstable-v1).
- `libei`: This backend uses [libei](https://gitlab.freedesktop.org/libinput/libei) and is supported by GNOME >= 45 or KDE Plasma >= 6.1.
- `windows`: Backend for input capture on Windows.
- `macos`: Backend for input capture on MacOS.
- `x11`: TODO (not yet supported)

## Roadmap

- [x] Graphical frontend (gtk + libadwaita)
- [x] Config file location follows `XDG_CONFIG_HOME`
- [x] IP address switching
- [x] Liveness tracking: ungrab the mouse when a client is unreachable, release keys when the server is offline
- [x] macOS key code translation
- [x] libei, macOS and Windows input capture
- [x] Encryption
- [x] Clipboard support (plain text)
- [ ] X11 input capture
- [ ] Latency measurement and visualization
- [ ] Bandwidth usage measurement and visualization
