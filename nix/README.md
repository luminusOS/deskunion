# Nix Flake Usage

## Run

```bash
nix run github:luminusOS/deskunion

# With params
nix run github:luminusOS/deskunion -- --help

```

## Home-manager module

Add input:

```nix
inputs = {
    deskunion.url = "github:luminusOS/deskunion";
}
```

Enable deskunion:

``` nix
{
  inputs,
  ...
}: {
  # Add the Home Manager module
  imports = [inputs.deskunion.homeManagerModules.default];

  programs.deskunion = {
    enable = true;
    # systemd = false;
    # package = inputs.deskunion.packages.${pkgs.stdenv.hostPlatform.system}.default
    # Optional configuration in nix syntax, see config.toml for available options
    # settings = { };
    };
  };
}

```
