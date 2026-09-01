# Relay fuzz targets

The configuration target exercises YAML deserialization and all semantic validators with arbitrary
UTF-8 input. It is intentionally outside the primary workspace so ordinary builds do not compile
libFuzzer.

```console
cargo install cargo-fuzz --locked
cargo fuzz run config_parse -- -max_len=65536
```

Run fuzzing on a Unix-like host with Clang. Preserve any crashing input as a normal regression test
before fixing the underlying defect.
