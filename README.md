## Build instructions


- Install windows protoc from [github](https://github.com/protocolbuffers/protobuf/releases)
- cargo build

## For MacOS

In shell do
```
export RUSTFLAGS="-C link-args=-ObjC"
export RUST_LOG=info
```

Then

```
cargo run
```