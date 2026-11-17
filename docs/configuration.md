# Configuration

DeskUnion reads `$XDG_CONFIG_HOME/deskunion/config.toml` on startup (`$XDG_CONFIG_HOME` defaults to `~/.config/`), and on Windows `%LOCALAPPDATA%\deskunion\config.toml`.

The GTK frontend sets the role under Settings, Role:

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

To create the file, copy this example:

## Example
> [!TIP]
> key symbols in the release bind are named according
> to their names in [crates/input-event/src/scancode.rs#L172](../crates/input-event/src/scancode.rs#L176).
> This is bound to change

```toml
# example configuration

# operation role (server | client). When absent it is inferred: a file
# with [[clients]] entries is a server, one with server_hostname/server_ips
# is a client, and an empty one is unconfigured. Set it explicitly.
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
# sha256 fingerprint of the server's certificate. Written on the first
# connection (trust on first use) and cleared when the endpoint above
# changes; a server showing another certificate is refused until you
# trust it in the app. Compare it with the fingerprint the server shows.
# server_fingerprint = "3f:2a:..."

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
# sha256 certificate fingerprint of the paired device - the stable
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
> dials out to clients - devices connect in and are matched by `fingerprint`.
> Both keys are still parsed for compatibility (and logged as deprecated) but
> are ignored; `hostname` is now only a display label.
