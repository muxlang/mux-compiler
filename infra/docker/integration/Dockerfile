# The CI image: a complete Rust + LLVM 22 environment for this repo.
#
# Two consumers, deliberately the same image so they cannot drift:
#   - docker-compose.integration.yml's `dev` service, which needs to reach the
#     postgres and echo-server containers.
#   - CI's bundled Linux job, which runs `container:` against the published
#     copy at ghcr.io/muxlang/mux-ci and so does not install LLVM, cargo-insta
#     or cargo-llvm-cov per job the way the split jobs each did.
#
# Published by .github/workflows/publish-ci-image.yml on any change to this
# file. Compose still builds it locally, so a local edit takes effect without
# waiting for a publish.
#
# Every tool version here is pinned. This image gates correctness, not just
# convenience, and a floating tool changes what CI enforces without any commit
# saying so.
FROM rust:1.93.1-bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        gnupg \
        lsb-release \
        make \
        pkg-config \
        python3 \
        software-properties-common \
        wget \
    && wget --max-redirect=0 -O /usr/share/keyrings/llvm-snapshot.gpg.key https://apt.llvm.org/llvm-snapshot.gpg.key \
    && echo "deb [signed-by=/usr/share/keyrings/llvm-snapshot.gpg.key] https://apt.llvm.org/$(lsb_release -cs)/ llvm-toolchain-$(lsb_release -cs)-22 main" > /etc/apt/sources.list.d/llvm.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        clang-22 \
        libpolly-22-dev \
        llvm-22-dev \
        bzip2 \
        libc6-dbg \
    && rustup component add clippy rustfmt llvm-tools-preview \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 1000 mux \
    && useradd --uid 1000 --gid mux --shell /bin/bash --create-home mux

# Valgrind is built from source at a pinned version rather than taken from apt.
#
# Debian bookworm ships 3.19, which names the writev parameter collectively
# ("writev(vector[...])"). Ubuntu 24.04 - what the GitHub runners used - ships
# 3.22, which names it per element ("writev(vector[0])"). Suppressions match
# that name LITERALLY, so infra/valgrind-programs.supp, written and validated
# against 3.22, silently stopped matching under 3.19 and the TLS false
# positives in test_std_http.mux began failing the gate.
#
# Both spellings were confirmed by running the same writev repro under each
# version. Pinning 3.22.0 reproduces the behaviour the suppression file was
# written for, and pins it - previously this tracked whatever the base image
# happened to carry, which is how the skew appeared in the first place.
#
# If this is ever bumped, re-check that the suppressions still match; the
# parameter naming is not a stable interface.
ARG VALGRIND_VERSION=3.22.0
ARG VALGRIND_SHA256=c811db5add2c5f729944caf47c4e7a65dcaabb9461e472b578765dd7bf6d2d4c
RUN set -eux; \
    cd /tmp; \
    curl -fsSL --proto '=https' -O "https://sourceware.org/pub/valgrind/valgrind-${VALGRIND_VERSION}.tar.bz2"; \
    echo "${VALGRIND_SHA256}  valgrind-${VALGRIND_VERSION}.tar.bz2" | sha256sum -c -; \
    tar -xf "valgrind-${VALGRIND_VERSION}.tar.bz2"; \
    cd "valgrind-${VALGRIND_VERSION}"; \
    ./configure --prefix=/usr/local >/dev/null; \
    make -j"$(nproc)" >/dev/null; \
    make install >/dev/null; \
    cd /; rm -rf "/tmp/valgrind-${VALGRIND_VERSION}" "/tmp/valgrind-${VALGRIND_VERSION}.tar.bz2"; \
    valgrind --version

# cargo-insta is matched to the `insta` library version in Cargo.lock (1.47.2).
# It gates correctness rather than reporting - `--unreferenced=reject` is what
# catches orphaned snapshots - so a release that changed what that flag means
# could stop enforcing without failing. Move this pin and the lockfile together.
#
# cargo-llvm-cov was previously installed unpinned in CI. Pinned here so the
# image is reproducible; llvm-tools-preview above is its prerequisite.
RUN cargo install cargo-insta --version 1.47.2 --locked \
    && cargo install cargo-llvm-cov --version 0.8.7 --locked \
    && rm -rf "${CARGO_HOME:-/usr/local/cargo}/registry" "${CARGO_HOME:-/usr/local/cargo}/git"

# No USER directive, on purpose. Compose sets `user:` explicitly for the dev
# service (LOCAL_UID/LOCAL_GID, so bind-mounted files keep the caller's
# ownership), and GitHub Actions needs to write to a workspace it mounts as
# root. Baking a non-root default would break the second without helping the
# first. The mux user still exists for compose to select.
ENV LLVM_SYS_221_PREFIX=/usr/lib/llvm-22 \
    LLVM_CONFIG_PATH=/usr/bin/llvm-config-22 \
    LLVM_SYS_221_STRICT_VERSIONING=1 \
    CC=clang-22 \
    CXX=clang++-22

WORKDIR /workspace
