---
title: "Install the TUI"
---

## Quick install (curl)

The install script detects your platform and downloads a prebuilt binary from GitHub Releases:

```bash
curl -fsSL https://raw.githubusercontent.com/stevengonsalvez/agents-in-a-box/main/ainb-tui/install.sh | bash
```

Prebuilt binaries are published for:

| Platform | Target |
|----------|--------|
| macOS (Apple Silicon) | `aarch64-apple-darwin` |
| Linux (x86_64) | `x86_64-unknown-linux-gnu` |

The script installs to `/usr/local/bin` when writable, otherwise `~/.local/bin` (add it to your `PATH` if prompted). Override the location with `INSTALL_DIR`, or pin a version with `VERSION`:

```bash
INSTALL_DIR=$HOME/bin VERSION=1.2.0 \
  curl -fsSL https://raw.githubusercontent.com/stevengonsalvez/agents-in-a-box/main/ainb-tui/install.sh | bash
```

The script verifies the release checksum when a `.sha256` file is present.

## From source (cargo)

Intel macOS and ARM64 Linux do not have prebuilt binaries — install from source instead. This also works on any platform with a Rust toolchain:

```bash
cargo install --git https://github.com/stevengonsalvez/agents-in-a-box --branch main ainb
```

Or build the workspace directly from a clone:

```bash
git clone https://github.com/stevengonsalvez/agents-in-a-box
cd agents-in-a-box/ainb-tui
cargo build --release   # binary at target/release/ainb
```

## Windows

Windows is not supported natively. Use WSL2 and follow the Linux instructions above.

## Verify the install

```bash
ainb --version
ainb init --check    # confirm prerequisites (git, tmux, a provider CLI)
```

## See also

- [Quickstart](quickstart.md)
- [Overview](overview.md)
- [Docs hub](../README.md)
