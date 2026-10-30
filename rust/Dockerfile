# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

# Toolchain to build the wasm module (see ../build.js): the crate is mounted in
# /code at run time.
FROM rust:latest

RUN rustup target add wasm32-unknown-unknown && \
    cargo install wasm-pack

# Binaryen for the JS fallback. Keep it outside PATH so wasm-pack can use
# its own wasm-opt.
RUN echo "deb http://deb.debian.org/debian unstable main" \
      > /etc/apt/sources.list.d/unstable.list && \
    apt-get update && \
    apt-get install -y --no-install-recommends -t unstable binaryen && \
    rm -rf /var/lib/apt/lists/* && \
    mkdir -p /opt/binaryen && \
    for tool in /usr/bin/wasm*; do mv "$tool" /opt/binaryen/; done
ENV BINARYEN=/opt/binaryen

WORKDIR /code
CMD ["/bin/sh", "-c", "/code/compile.sh"]
