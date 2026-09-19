# PowGrid Reticulum AI ($RAIX) CPU Miner

[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20Windows%20%7C%20macOS-lightgrey.svg)]()
[![Engine](https://img.shields.io/badge/engine-AVX2%20%7C%20SHA--NI%20%7C%20NEON-orange.svg)]()

High-performance, multi-threaded CPU miner for **Reticulum AI ($RAIX)** built from scratch in pure native Rust with SIMD hardware acceleration (x86_64 AVX2/SHA-NI and Apple Silicon ARM64 NEON).

---

## Key Features

- **Multi-Platform Support**: Runs natively on Linux (x86_64), Windows (x64), and macOS (Apple Silicon M1/M2/M3/M4 & Intel).
- **SIMD Hardware Acceleration**: Automatic utilization of x86_64 SHA-NI and AVX2 instructions or ARM64 NEON extensions for peak hashing throughput.
- **Dynamic Thread Scaling**: Automatically detects physical CPU cores and distributes multi-threaded hashing loops with zero heap reallocation.
- **Real-Time ANSI Telemetry**: Mathematical 78-column terminal HUD displaying live total hashrate, accepted shares, latency, and current network block.
- **Pure Rust Reliability**: Zero unsafe memory overhead, no Python or external runtime dependencies.

---

## Building from Source

### 1. Prerequisites

- **Rust & Cargo**: [https://rustup.rs/](https://rustup.rs/) (version 1.75+)
- **Build Tools**: Standard C toolchain (`build-essential` on Debian/Ubuntu, Visual Studio Build Tools on Windows, Xcode CLI Tools on macOS).

### 2. Compile Release Binary

```bash
git clone https://github.com/powgrid/powgrid-raix-cpu-miner.git
cd powgrid-raix-cpu-miner

# Build with maximum CPU instruction set optimization
RUSTFLAGS="-C target-cpu=native" cargo build --release
```

The compiled binary will be located at:
```
target/release/powgrid-raix-cpu-miner
```

---

## Usage & Command-Line Arguments

### Quick Start

```bash
./powgrid-raix-cpu-miner \
  --address <YOUR_RAIX_WALLET_ADDRESS> \
  --worker <WORKER_NAME> \
  --node https://raix.powgrid.xyz
```

### Options

| Flag | Description | Default |
| :--- | :--- | :--- |
| `--address <ADDR>` | Your Reticulum AI wallet address (`ctx1...`) | *Required* |
| `--worker <NAME>` | Worker identifier for pool stats | `cpu-worker-1` |
| `--node <URL>` | Mining pool or node RPC endpoint | `https://raix.powgrid.xyz` |
| `--threads <N>` | Number of concurrent CPU mining threads | Number of CPU cores |

### Example

```bash
./powgrid-raix-cpu-miner --address ctx1abc... --worker my-ryzen-cpu --threads 16
```

---

## License

Dual-licensed under either:
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
