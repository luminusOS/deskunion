# Usage

## Roles

The machine whose mouse and keyboard you want to share runs in server mode. It listens for connections on UDP port `4242`, which is configurable. Each machine you want to control runs in client mode: enter the server's host name or IP address and port in the Server section and click Connect. The client only dials out, so it works behind NAT and needs no firewall changes.

On a fresh installation, choose "Share this keyboard and mouse" (server) or "Use another computer's keyboard" (client) before sharing starts. Existing configurations keep their role. Change it later under Settings, Role. In narrow windows, such as on a phone, the sidebar fills the window and each page opens over it with a back button.

## Pairing

When a client connects for the first time, the server shows an authorization dialog with the client's certificate fingerprint. The client shows the same fingerprint under Settings, This computer. Authorize it and the device is paired and placed on the first free screen edge, preferring the right one (right, then left, top, bottom). The binding is saved in the configuration file, and you can move the device to another edge from the Connections page.

When all four edges are taken, the device appears under "Waiting for a position" on the Connections page until you pick an edge.

Paired clients announce their computer name over the authenticated connection. The server fills an empty display label with it, and keeps any label you set.

If a device cannot connect, check that UDP port `4242` (or the port you chose) is open in the server's firewall. On Windows, `windows-firewall.ps1` from the release ZIP does this, and it also clears the block rule that a dismissed Defender prompt leaves behind.

## Connection list

Each connected computer shows its round trip time in milliseconds next to a signal icon. The server probes every client every 5 seconds, and the icon follows the last answer: up to 20 ms is excellent, up to 50 ms good, up to 120 ms fair, and anything above is weak. A computer that is not connected shows neither, and it gets no screen-edge barrier: on Linux the portal does not capture the pointer at the edge of an offline computer.

## Connection retry

A client that cannot reach its server keeps trying on Deskflow's schedule: every second for the first 300 attempts, then every 5, 10, 30, 60 and 120 seconds, and finally every 5 minutes. A session that drops starts over at one second. The server's host name is resolved again before each attempt, so a late network or a moved server is picked up. While the server waits for this computer to be authorized, the client retries every 15 seconds without backing off. The window shows the delay before the next attempt.

## Tray icon

Closing the window keeps DeskUnion running while a tray icon is available:

- Linux: a StatusNotifierItem (KDE Plasma and other hosts). GNOME needs the AppIndicator extension, and without a tray host closing the window quits the app. In a Flatpak, the Background Apps portal keeps the app running and GNOME lists it under Background Apps.
- Windows: a notification area icon.
- macOS: a menu bar item.

Click the icon to show or hide the window. The menu offers, depending on the mode:

- Server: start or stop sharing, and a Computers submenu to turn input sharing on or off for each paired computer.
- Client: connect or disconnect, and "Reconnect now", which skips the retry delay.
- Both modes: show or hide the window, and quit.

The tooltip shows the current status, such as "Client · retrying in 5 s".

## Clipboard

Plain-text clipboard sharing works between a Windows client and a GNOME Wayland server. It is on by default. To turn it off, use the Clipboard switch on the Settings page, or set this in `config.toml` and restart DeskUnion:

```toml
[clipboard]
enabled = false
```

On GNOME, the clipboard permission is requested from the same portal prompt as input capture. It needs an InputCapture portal that supports the v2 session and grants clipboard permission. Older portals keep input sharing but have no clipboard sync. Transfers are UTF-8 text up to 64 KiB, and images and files are not shared.

Text is exchanged while the other computer is the active one: copy on either side, then move the cursor across. Only copies made while DeskUnion runs are shared, so whatever was on the Windows clipboard at startup is not sent. Each step is logged at info level with a `clipboard:` prefix. Find a missing transfer with `journalctl --user | grep clipboard:` or in `%LOCALAPPDATA%\deskunion\deskunion.log`.

## Audio

See [audio.md](audio.md).

## Command line

```sh
deskunion cli help          # list the commands
deskunion cli <cmd> help    # help for one command
```

## Daemon and systemd

`deskunion daemon` runs DeskUnion in the background, for example under systemd. To start it with the graphical session, use the [user service](../service/deskunion.service):

```sh
cp service/deskunion.service ~/.config/systemd/user
systemctl --user daemon-reload
systemctl --user enable --now deskunion.service
```

> [!IMPORTANT]
> Point `ExecStart=/usr/bin/deskunion daemon` at the real `deskunion` binary if it is not under `/usr/bin`, for example after a manual install.

## Windows cursor

While another computer is controlled, a Windows server parks the cursor at the center of the display it left from, so motion is never lost against the screen edge. When control returns, the cursor reappears two pixels inside the edge it crossed, at the height it left. The capture thread works in physical pixels, so this also holds with display scaling above 100%. The cursor is not hidden, and the return position does not follow where it left the other computer. Set `DESKUNION_PARK_CURSOR=0` to keep the cursor at the edge instead.
