# Quick Start

Use `sc-nixgen` to inspect the machine and produce a starter NixOS configuration:

```sh
nix run .#sc-nixgen -- detect --output smartcool.nix
```

Include motherboard embedded-controller temperature sensors when they are useful for the host:

```sh
nix run .#sc-nixgen -- detect --include-ec --output smartcool.nix
```

Review the generated file before importing it. The detector assigns conservative defaults from the visible hwmon devices, but fan topology and curves are machine-specific.

After enabling the generated NixOS module configuration and rebuilding the host, check the daemon through the CLI:

```sh
sc status
sc analytics
sc tuning
```

Validate a RON configuration directly with:

```sh
sc config --validate /etc/smartcool/config.ron
```
