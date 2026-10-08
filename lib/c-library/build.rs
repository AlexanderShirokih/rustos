//! Компонует библиотеку C и компилирует системный слой файлов и значения `errno` и
//! `pthread.h` из её заголовков.

fn main() {
    userland_build::compile_c(
        "c-library",
        &["src/errno.c", "src/files.c", "src/pthread.c"],
    );
    userland_build::link_libc();
}
