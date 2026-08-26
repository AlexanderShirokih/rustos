# CI/dev-образ для сборки ядра. Содержит только окружение: toolchain и
# системные инструменты.
FROM debian:bookworm-slim

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH \
    CARGO_TERM_COLOR=always

# Системные зависимости:
#   qemu-system-arm   — запуск QEMU integration tests (xtask qemu-test)
#   gcc/g++/libc6-dev — линкер для host-сборок, build-скриптов и libFuzzer
#   git/ca-certs/curl — checkout, rustup, загрузка crates
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        gcc \
        g++ \
        git \
        libc6-dev \
        qemu-system-arm \
    && rm -rf /var/lib/apt/lists/*

# rustup без дефолтного toolchain — версию диктует rust-toolchain.toml ниже.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --default-toolchain none --profile minimal

# Ставим зафиксированный nightly + компоненты + target из rust-toolchain.toml.
# Копируем только этот файл, чтобы слой переиспользовался, пока пин не менялся.
# `rustup show` в каталоге с rust-toolchain.toml триггерит установку нужного
# toolchain со всеми component'ами и target'ами.
COPY rust-toolchain.toml /tmp/rust-toolchain.toml
RUN cd /tmp \
    && rustup show \
    && rustup default "$(rustup show active-toolchain | cut -d' ' -f1)" \
    && rm rust-toolchain.toml

# cargo-binutils нужен для `cargo objcopy` внутри `xtask build` / `qemu-test`.
# cargo-fuzz собирает и запускает fuzz-targets в CI.
RUN cargo install cargo-binutils --locked \
    && cargo install cargo-fuzz --locked

# sccache служит обёрткой компилятора: кэш монтируют снаружи, обёртка принадлежит образу.
# Готовая сборка вместо `cargo install`: сборка из исходников требует заголовков OpenSSL.
ARG SCCACHE_VERSION=0.17.0
RUN case "$(dpkg --print-architecture)" in \
        amd64) target=x86_64-unknown-linux-musl ;; \
        arm64) target=aarch64-unknown-linux-musl ;; \
        *) echo "unsupported architecture: $(dpkg --print-architecture)" >&2; exit 1 ;; \
    esac \
    && name="sccache-v${SCCACHE_VERSION}-${target}" \
    && curl --proto '=https' --tlsv1.2 -sSfL \
        "https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/${name}.tar.gz" \
        | tar -xz -C /tmp \
    && install -m 0755 "/tmp/${name}/sccache" /usr/local/bin/sccache \
    && rm -rf "/tmp/${name}"

ARG SIMILARITY_RS_VERSION=0.5.0
RUN cargo install similarity-rs --locked --version "$SIMILARITY_RS_VERSION"

WORKDIR /work
