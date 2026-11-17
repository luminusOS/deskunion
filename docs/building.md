# Building from source

## Dependencies

macOS:

```sh
brew install libadwaita pkg-config imagemagick
cargo install cargo-bundle
```

Ubuntu and derivatives:

```sh
sudo apt install libadwaita-1-dev libgtk-4-dev libx11-dev libxtst-dev
```

Arch and derivatives:

```sh
sudo pacman -S libadwaita gtk libx11 libxtst
```

Fedora and derivatives:

```sh
sudo dnf install libadwaita-devel libXtst-devel libX11-devel
```

Nix:

```sh
nix-shell .   # or: nix develop
```

### Windows

Install [Rust](https://www.rust-lang.org/tools/install), then build GTK with gvsbuild, as described in the [gtk-rs book](https://gtk-rs.org/gtk4-rs/stable/latest/book/installation_windows.html).

In an admin PowerShell:

```powershell
# install chocolatey
Set-ExecutionPolicy Bypass -Scope Process -Force; iex ((New-Object System.Net.WebClient).DownloadString('https://community.chocolatey.org/install.ps1'))

# install gvsbuild dependencies
choco install python git msys2 visualstudio2022-workload-vctools
```

In a regular PowerShell:

```powershell
python -m pip install --user pipx
python -m pipx ensurepath
```

Relaunch PowerShell so the environment updates, then:

```powershell
pipx install gvsbuild
gvsbuild build gtk4 libadwaita librsvg adwaita-icon-theme
```

Add `C:\gtk-build\gtk\x64\release\bin` to the `PATH` environment variable, or the project fails to build. To skip building GTK, disable the gtk frontend (see [conditional compilation](#conditional-compilation)).

## Compiling

```sh
# release build
cargo build --release

# run the application crate
cargo run -p deskunion-app

# install into ~/.cargo/bin
cargo install deskunion-app

# install by hand
sudo cp target/release/deskunion /usr/local/bin/
```

With Nix, `nix-build` leaves the executable in `result/bin/deskunion`.

### Packages

```sh
# Linux packages, including the AppImage
cargo install cargo-bundle
cargo bundle --release
# output: target/release/bundle/appimage/deskunion_*.AppImage
```

On macOS, build the bundle like this:

```sh
scripts/makeicns.sh            # create the icon file
cargo bundle                   # create the .app bundle
scripts/copy-macos-dylib.sh    # copy dynamic libraries into the bundle
```

## Conditional compilation

Support for other platforms is left out automatically based on the active Rust toolchain. Backends and frontends can also be chosen with [cargo features](https://doc.rust-lang.org/cargo/reference/features.html). For example, this builds only the `layer-shell` capture backend and the `wlroots` emulation backend, which is enough for Sway:

```sh
cargo build --no-default-features --features layer_shell_capture,wlroots_emulation
```

`--no-default-features` also drops audio streaming, which is behind the `audio` feature. The full feature list is in [Cargo.toml](../Cargo.toml).

## Git pre-commit hook

`.githooks/pre-commit` runs `cargo fmt --all` (and fails if files changed), `cargo clippy --workspace --all-targets --all-features -- -D warnings` and `cargo test --workspace --all-features`. Enabling it is optional:

```sh
chmod +x .githooks/pre-commit
git config core.hooksPath .githooks
```

See [ARCHITECTURE.md](../ARCHITECTURE.md) for how the crates fit together and how to run the graphical test without a desktop.
