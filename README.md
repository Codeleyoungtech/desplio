# Desplio

Desplio is a Linux-first virtual display streaming platform that turns networked devices into real external monitors with bidirectional input.

This repository is currently in early milestone development. The active focus is:

- `M0`: evdi-backed virtual display creation
- Linux host daemon in Rust
- Monorepo structure for desktop, web, and mobile clients

## Repository Layout

```text
apps/
  daemon/          Rust host daemon
  tray/            Tauri tray app
  web-client/      Browser client
  mobile-client/   Capacitor mobile client
  desktop-client/  Tauri desktop client
packages/
  client-core/     Shared client logic
  ui/              Shared UI package
kernel/
  evdi-builder/    Kernel/display integration helpers
scripts/
  install.sh       Ubuntu host setup
  dev.sh           Daemon dev runner
```

## Current Status

The workspace and first `evdi` daemon path are in place. M0 is currently focused on making a real virtual monitor appear in the compositor/X11 output list.

## Getting Started

Install host dependencies:

```bash
sudo ./scripts/install.sh
```

Run the daemon:

```bash
cargo run -p desplio-daemon
```

Verify the display on X11:

```bash
xrandr --listproviders
xrandr --query
```

## Development

- Rust workspace root: `Cargo.toml`
- npm workspace root: `package.json`
- Primary PRD: `Desplio_PRD_v1.0.md`

## Open Source

This repo is being prepared to support open-source development. Governance and contribution policy are intentionally lightweight for now and can evolve as the project stabilizes.
