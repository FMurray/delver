#!/bin/sh
cargo zigbuild -p viewer --target wasm32-unknown-unknown --release
trunk serve
