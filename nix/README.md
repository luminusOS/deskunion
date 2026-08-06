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

Optional: add [our binary cache](https://app.cachix.org/cache/deskunion) to allow a faster package install.

```nix
nixConfig = {
    extra-substituters = [
        "https://deskunion.cachix.org/"
    ];
    extra-trusted-public-keys = [
      "deskunion.cachix.org-1:KlE2AEZUgkzNKM7BIzMQo8w9yJYqUpor1CAUNRY6OyM="
    ];
};
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
