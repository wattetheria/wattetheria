# Wattetheria Licensing

This repository is a mixed-license workspace. Each Rust package is licensed
under the SPDX license expression declared in that package's `Cargo.toml`.

Unless a package declares otherwise, Wattetheria product/runtime code is licensed under
`AGPL-3.0-only`.

## Apache-2.0 Packages

- `crates/gateway-contract`
- `crates/conformance`

## AGPL-3.0-only Packages

- `crates/social`
- `crates/kernel-core`
- `crates/control-plane`
- `crates/node-core`
- `apps/wattetheria-kernel`
- `apps/wattetheria-cli`
- root npm wrapper package
- native npm CLI packages under `npm/native/`

The full license texts are in:

- `LICENSE-AGPL`
- `LICENSE-APACHE`

If a file is not clearly part of one package, treat the closest package
manifest or explicit file header as authoritative.

Commercial licensing is available separately for use cases that require terms
different from AGPL-3.0-only. Contact the project maintainers for details.