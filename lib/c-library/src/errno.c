/* Значения errno из заголовков библиотеки C для Rust-части c-library. */

#include <errno.h>

const int c_library_ebadf = EBADF;
const int c_library_einval = EINVAL;
const int c_library_eio = EIO;
const int c_library_enomem = ENOMEM;
const int c_library_espipe = ESPIPE;
const int c_library_enoent = ENOENT;
const int c_library_enotdir = ENOTDIR;
const int c_library_eisdir = EISDIR;
const int c_library_enotempty = ENOTEMPTY;
const int c_library_eexist = EEXIST;
const int c_library_emfile = EMFILE;
const int c_library_erofs = EROFS;
const int c_library_enospc = ENOSPC;
const int c_library_exdev = EXDEV;
const int c_library_enotsup = ENOTSUP;
const int c_library_etimedout = ETIMEDOUT;
const int c_library_enotconn = ENOTCONN;
const int c_library_eagain = EAGAIN;
const int c_library_edeadlk = EDEADLK;
const int c_library_ebusy = EBUSY;
const int c_library_eperm = EPERM;
const int c_library_enosys = ENOSYS;
