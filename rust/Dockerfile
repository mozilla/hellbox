# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

# Toolchain to build the wasm module (see ../build.js): the crate is mounted in
# /code at run time.
FROM rust:latest

RUN rustup target add wasm32-unknown-unknown && \
    cargo install wasm-pack

WORKDIR /code
CMD ["/bin/sh", "-c", "/code/compile.sh"]
