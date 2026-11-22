# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

# Toolchain to build the wasm module (see ../build.js): the repository is
# mounted in /code at run time.

# Node.js and terser, which minifies the JS fallback, from package-lock.json.
FROM node:26.13.1-trixie-slim@sha256:e6bedef217677b276c591795e169840edd9096dbf13f29df8ef8d1856b8820bc AS node
WORKDIR /opt/tools
COPY package.json package-lock.json ./
RUN npm ci --ignore-scripts --no-audit --no-fund

FROM rust:1.100.0@sha256:223cc32ddca8f3ee90c436dbc94b6b5f052ea7f9653c5201e9612f08ad7d21b8

# Keep wasm-pack in sync with CI.
RUN rustup target add wasm32-unknown-unknown && \
    cargo install --locked wasm-pack@0.15.1

# Binaryen for the JS fallback. Keep it outside PATH so wasm-pack can use
# its own wasm-opt.
RUN set -eu; \
    arch="$(uname -m)"; \
    case "${arch}" in \
      x86_64) sha256=2dc9c7813f5375db93d96ead4b78222fcc3e2677bbb832297af4797782a37489;; \
      aarch64) sha256=89c07ea56faf38d0fbecf36ca8ec0721756716185f265b568e133d427f299bf8;; \
      *) echo "Unsupported architecture: ${arch}" >&2; exit 1;; \
    esac; \
    release=version_133; \
    archive="binaryen-${release}-${arch}-linux.tar.gz"; \
    curl -fsSLo "/tmp/${archive}" \
      "https://github.com/WebAssembly/binaryen/releases/download/${release}/${archive}"; \
    echo "${sha256}  /tmp/${archive}" | sha256sum -c -; \
    mkdir -p /opt/binaryen; \
    tar -xzf "/tmp/${archive}" -C /opt/binaryen --strip-components=2 \
      "binaryen-${release}/bin"; \
    rm "/tmp/${archive}"
ENV BINARYEN=/opt/binaryen

# Node.js runs terser and adds the build metadata.
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /opt/tools /opt/tools
ENV TERSER=/opt/tools/node_modules/.bin/terser

WORKDIR /code/rust
CMD ["/bin/sh", "-c", "/code/rust/compile.sh"]
