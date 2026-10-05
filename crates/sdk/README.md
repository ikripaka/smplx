# smplx-sdk-test

The `smplx-sdk-test` crate is a standalone set of modules of a larger [Smplx](https://github.com/ikripaka/smplx) framework that can be used separately to interact with Simplicity smart contracts.

It also streamlines building, signing, and broadcasting transactions on Liquid.

## Functionality

- `signer` - Securely parse BIP39 mnemonics, manage keys, sign transactions, and work with confidential addresses.
- `provider` - Connect to existing Elements nodes via RPC or Esplora APIs to query UTXOs and broadcast transactions.
- `transaction` - High-level builder abstractions over `FinalTransaction`, `UTXO`, `PartialInput`, and `PartialOutput`.
- `program` - Load and interact with Simplicity (`.simf`) smart contracts.

The `smplx-sdk-test` can be used as a standalone SDK, however, check out [Smplx](https://github.com/ikripaka/smplx) for a complete Simplicity development experience.

## Features

By default, `smplx-sdk` is wasm-incompatible with provider functionality turned on. Disable a `provider` feature to make the SDK wasm-compatible.

## Quick Start

Read [simplex/README.md](https://github.com/ikripaka/smplx/blob/master/README.md).

## Disclaimer

Secure DeFi. On Bitcoin.
