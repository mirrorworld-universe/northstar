# NorthStar

NorthStar is the validator client for the NorthStar chain, maintained by
Sonic SVM. It is derived from
[Agave](https://github.com/anza-xyz/agave) — the Solana validator implementation
by [Anza](https://www.anza.xyz/) — with extensions specific to NorthStar's
runtime.

# Resources

- **Website:** [northstar.sonicsvm.org](https://northstar.sonicsvm.org)
- **Developer docs:** [docs.northstar.sonicsvm.org](https://docs.northstar.sonicsvm.org)
- **Technical litepaper:** [NorthStar — Programmable On-Demand Runtimes for Solana](https://github.com/mirrorworld-universe/reports/blob/master/NorthStar%20%E2%80%93%20Programmable%20On-Demand%20Runtimes%20for%20Solana.pdf)

# Building

## **1. Install rustc, cargo and rustfmt.**

```bash
$ curl https://sh.rustup.rs -sSf | sh
$ source $HOME/.cargo/env
$ rustup component add rustfmt
```

The `rust-toolchain.toml` file pins a specific rust version and ensures that
cargo commands run with that version. Note that cargo will automatically install
the correct version if it is not already installed.

On Linux systems you may need to install libssl-dev, pkg-config, zlib1g-dev, protobuf etc.

On Ubuntu:
```bash
$ sudo apt-get update
$ sudo apt-get install libssl-dev libudev-dev pkg-config zlib1g-dev llvm clang cmake make libprotobuf-dev protobuf-compiler libclang-dev
```

On Fedora:
```bash
$ sudo dnf install openssl-devel systemd-devel pkg-config zlib-devel llvm clang cmake make protobuf-devel protobuf-compiler perl-core libclang-dev
```

## **2. Download the source code.**

```bash
$ git clone https://github.com/mirrorworld-universe/northstar.git
$ cd northstar
```

## **3. Build.**

```bash
$ ./cargo build
```

> [!NOTE]
> Note that this builds a debug version that is **not suitable for running a testnet or mainnet validator**. Please read [`docs/src/cli/install.md`](docs/src/cli/install.md#build-from-source) for instructions to build a release version for test and production uses.

# Testing

**Run the test suite:**

```bash
$ ./cargo test
```

# Benchmarking

First, install the nightly build of rustc. `cargo bench` requires the use of the
unstable features only available in the nightly build.

```bash
$ rustup install nightly
```

Run the benchmarks:

```bash
$ cargo +nightly bench
```

# Release Process

The release process for this project is described [here](RELEASE.md).

# Code coverage

To generate code coverage statistics:

```bash
$ scripts/coverage.sh
$ open target/cov/lcov-local/index.html
```

# License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for the
full license text.

# Acknowledgments

NorthStar is built on top of the [Agave](https://github.com/anza-xyz/agave)
validator client. We are grateful to Anza, Solana Labs, and the wider Solana
open-source community whose work makes this project possible.
