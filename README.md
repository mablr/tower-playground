# Tower Playground

A collection of examples demonstrating how to create layered services with the [Tower](https://github.com/tower-rs/tower) library in Rust. This project serves as a learning resource for understanding Tower's middleware patterns and implementation techniques.

This layered service pattern is also the foundation of [Hyper](https://github.com/hyperium/hyper) and [Tonic](https://github.com/hyperium/tonic) frameworks.

## Examples

```bash
# Echo server with auth and timing middleware
cargo run --example echo_auth_timing

# IO mediation example
cargo run --example io_mediation

# Restaurant reservation system
cargo run --example restaurant_reservations
```