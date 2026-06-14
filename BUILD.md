# Сборка RustOS Mobile

Сборка, запуск и тесты идут через `cargo xtask`. Быстрый старт:

```bash
cargo xtask build devices/spec/qemu-aarch64.yaml --run
```

Подробности — в документации:

- [docs/environment.md](docs/environment.md) — toolchain и host-зависимости,
  device specs;
- [docs/commands.md](docs/commands.md) — команды сборки, запуска и тестов,
  артефакты под разные `boot.format`.
