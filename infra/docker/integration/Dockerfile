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
    && rustup component add clippy rustfmt \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 1000 mux \
    && useradd --uid 1000 --gid mux --shell /bin/bash --create-home mux

USER mux

ENV LLVM_SYS_221_PREFIX=/usr/lib/llvm-22 \
    LLVM_CONFIG_PATH=/usr/bin/llvm-config-22 \
    LLVM_SYS_221_STRICT_VERSIONING=1 \
    CC=clang-22 \
    CXX=clang++-22

WORKDIR /workspace
