# Contributing

Thanks for contributing to Desplio.

## Ground Rules

- Keep changes scoped and readable.
- Prefer small PRs over large multi-purpose changes.
- Discuss architecture changes before implementing them.
- Do not commit secrets, certificates, local IPs, or machine-specific config.

## Setup

```bash
cargo check --workspace
npm pkg get workspaces
```

For Linux host work, document:

- distro and version
- kernel version
- X11 or Wayland session
- GPU/DRM environment

## Pull Requests

- Explain the user-visible effect.
- Include validation steps.
- Call out platform assumptions and known gaps.

## Safety

Desplio touches display, input, and network surfaces. Please be deliberate with:

- kernel-module setup
- input injection
- local-network security
- permissions and privilege boundaries
