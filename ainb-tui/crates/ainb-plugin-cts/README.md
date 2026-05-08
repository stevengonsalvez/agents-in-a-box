# ainb-plugin-cts

Conformance Test Suite for [agents-in-a-box](https://github.com/stevengonsalvez/agents-in-a-box) plugins.

`run_contract_v1(manifest_toml, wasm)` returns a `ConformanceReport`
asserting your plugin honours the v1 spec.

## Usage

In your plugin crate's `Cargo.toml`:

```toml
[dev-dependencies]
ainb-plugin-cts = "1"
```

In `tests/conformance.rs`:

```rust
use ainb_plugin_cts::run_contract_v1_self_build;

#[test]
fn passes_v1_contract() {
    // Builds your plugin (wasm32-wasip1), then runs every axis.
    let manifest = include_str!("../plugin.toml");
    let report = run_contract_v1_self_build("my-plugin", manifest)
        .expect("self-build harness");
    assert!(report.is_passing(), "{report}");
}
```

If you already have a built wasm, skip the self-build helper:

```rust
let manifest = include_str!("../plugin.toml");
let wasm = std::fs::read("target/wasm32-wasip1/release/my_plugin.wasm").unwrap();
let report = ainb_plugin_cts::run_contract_v1(manifest, &wasm);
assert!(report.is_passing(), "{report}");
```

## What's checked

Eight axes, defined in [`spec/v1/contract.toml`](spec/v1/contract.toml):

| # | Axis                                                     |
| - | -------------------------------------------------------- |
| 1 | manifest schema                                          |
| 2 | init lifecycle                                           |
| 3 | render: buffer dims + encoding                           |
| 4 | capability declarations match imports                    |
| 5 | event handling: alloc + handle\_event roundtrip          |
| 6 | shutdown idempotent                                      |
| 7 | version: `ainb_min_version` is semver                    |
| 8 | no panics during 100 random event sequences (stub in v1) |

The full human-readable spec lives at
[`docs/plugin-spec/v1.md`](https://github.com/stevengonsalvez/agents-in-a-box/blob/main/docs/plugin-spec/v1.md).

## Versioning

Tracks the contract version. `ainb-plugin-cts 1.x` validates
`contract_version = "v1"`. A `2.x` release will validate v2
(breaking) — and v1 will keep getting patch fixes alongside until
the v1 host floor moves on.
