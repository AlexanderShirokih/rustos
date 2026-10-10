# CI/dev-образ для сборки ядра. Содержит только окружение: toolchain и
# системные инструменты.
FROM debian:bookworm-slim

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH \
    CARGO_TERM_COLOR=always

# Системные зависимости:
#   qemu-system-arm   — запуск QEMU integration tests (xtask qemu-test); версия из
#                       bookworm-backports нужна для запуска virtio-multitouch-device
#   gcc/g++/libc6-dev — линкер для host-сборок, build-скриптов и libFuzzer
#   git/ca-certs/curl — checkout, rustup, загрузка crates
#   meson/ninja/xz    — сборка picolibc из релизного архива (xtask libc-src)
#   cmake             — сборка рантаймов C++ LLVM из релизного архива (xtask libc-src)
#   unzip             — распаковка архивов стороннего кода (xtask third-party)
RUN echo "deb http://deb.debian.org/debian bookworm-backports main" \
        > /etc/apt/sources.list.d/bookworm-backports.list \
    && apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        cmake \
        curl \
        gcc \
        g++ \
        git \
        libc6-dev \
        meson \
        ninja-build \
        unzip \
        xz-utils \
    && apt-get install -y --no-install-recommends -t bookworm-backports qemu-system-arm \
    && rm -rf /var/lib/apt/lists/*

# clang компилирует picolibc и C-исходники программ userland. Версия не ниже
# `MIN_CLANG_MAJOR` из xtask/src/libc_src.rs, поэтому пакет берётся из репозитория LLVM.
# meson проверяет компоновщик кросс-компилятора, и cross-файл задаёт ему ld.lld. Без llvm-ar
# архиватором остаётся GNU ar из binutils.
ARG LLVM_VERSION=18
RUN install -d /etc/apt/keyrings \
    && curl --proto '=https' --tlsv1.2 -sSfL https://apt.llvm.org/llvm-snapshot.gpg.key \
        -o /etc/apt/keyrings/llvm.asc \
    && echo "deb [signed-by=/etc/apt/keyrings/llvm.asc] https://apt.llvm.org/bookworm/ llvm-toolchain-bookworm-${LLVM_VERSION} main" \
        > /etc/apt/sources.list.d/llvm.list \
    && apt-get update && apt-get install -y --no-install-recommends "clang-${LLVM_VERSION}" "lld-${LLVM_VERSION}" \
    && ln -s "/usr/bin/clang-${LLVM_VERSION}" /usr/local/bin/clang \
    && ln -s "/usr/bin/ld.lld-${LLVM_VERSION}" /usr/local/bin/ld.lld \
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
