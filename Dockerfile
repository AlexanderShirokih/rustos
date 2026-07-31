# CI/dev-образ для сборки ядра.
#
# Содержит ТОЛЬКО окружение (toolchain + системные инструменты), но не
# компилированные зависимости и не target/: они path-зависимы и инвалидируются
# исходниками — их кэширует Swatinem/rust-cache в CI.
#
# Пересобирается только при изменении Dockerfile или rust-toolchain.toml
# (см. .github/workflows/ci-image.yml).
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

WORKDIR /work
