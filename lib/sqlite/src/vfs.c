/*
 * VFS `molecular` поверх файлов, часов и случайных байт библиотеки C и инициализация sqlite
 * процесса.
 *
 * VFS открывает базу и журнал по абсолютному пути от файлового корня процесса. Путь без
 * ведущего `/` даёт `SQLITE_CANTOPEN`. Чтение за концом файла заполняет остаток буфера нулями и
 * даёт `SQLITE_IOERR_SHORT_READ`. Сброс на носитель и замки завершаются успешно сразу: запись
 * возвращается после записи сервером, а файл базы должен использовать одно соединение.
 *
 * Конструктор до C-функции `main` ставит мьютексы sqlite поверх `pthread_mutex` и
 * инициализирует sqlite, который регистрирует VFS `molecular` по умолчанию.
 */

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#include "mutex.h"
#include "sqlite3.h"

#define SECTOR_BYTES 4096

/* Предел длины одного запроса `getentropy`. */
#define ENTROPY_CHUNK_BYTES 256

#define NS_PER_MS        1000000L
#define MS_PER_SEC       1000LL
#define US_PER_SEC       1000000
#define NS_PER_US        1000L

/* Юлианский день начала эпохи Unix в миллисекундах. */
#define UNIX_EPOCH_JULIAN_MS 210866760000000LL

#define FILE_MODE 0644

struct molecular_file {
    sqlite3_file base;
    int          fd;
};

static int descriptor(sqlite3_file *file)
{
    return ((struct molecular_file *)file)->fd;
}

static int seek_to(int fd, sqlite3_int64 offset)
{
    return lseek(fd, (off_t)offset, SEEK_SET) == (off_t)offset;
}

static int file_close(sqlite3_file *file)
{
    return close(descriptor(file)) == 0 ? SQLITE_OK : SQLITE_IOERR_CLOSE;
}

static int file_read(sqlite3_file *file, void *buffer, int amount, sqlite3_int64 offset)
{
    int fd = descriptor(file);
    if (!seek_to(fd, offset)) {
        return SQLITE_IOERR_READ;
    }

    unsigned char *bytes = buffer;
    int            done = 0;
    while (done < amount) {
        ssize_t count = read(fd, bytes + done, (size_t)(amount - done));
        if (count < 0) {
            return SQLITE_IOERR_READ;
        }
        if (count == 0) {
            break;
        }
        done += (int)count;
    }

    if (done < amount) {
        memset(bytes + done, 0, (size_t)(amount - done));
        return SQLITE_IOERR_SHORT_READ;
    }
    return SQLITE_OK;
}

static int file_write(sqlite3_file *file, const void *buffer, int amount, sqlite3_int64 offset)
{
    int fd = descriptor(file);
    if (!seek_to(fd, offset)) {
        return SQLITE_IOERR_WRITE;
    }

    const unsigned char *bytes = buffer;
    int                  done = 0;
    while (done < amount) {
        ssize_t count = write(fd, bytes + done, (size_t)(amount - done));
        if (count <= 0) {
            return SQLITE_IOERR_WRITE;
        }
        done += (int)count;
    }
    return SQLITE_OK;
}

static int file_truncate(sqlite3_file *file, sqlite3_int64 size)
{
    return ftruncate(descriptor(file), (off_t)size) == 0 ? SQLITE_OK : SQLITE_IOERR_TRUNCATE;
}

static int file_sync(sqlite3_file *file, int flags)
{
    (void)file;
    (void)flags;
    return SQLITE_OK;
}

static int file_size(sqlite3_file *file, sqlite3_int64 *size)
{
    struct stat st;
    if (fstat(descriptor(file), &st) != 0) {
        return SQLITE_IOERR_FSTAT;
    }
    *size = st.st_size;
    return SQLITE_OK;
}

static int file_lock(sqlite3_file *file, int level)
{
    (void)file;
    (void)level;
    return SQLITE_OK;
}

static int file_check_reserved_lock(sqlite3_file *file, int *reserved)
{
    (void)file;
    *reserved = 0;
    return SQLITE_OK;
}

static int file_control(sqlite3_file *file, int op, void *argument)
{
    (void)file;
    (void)op;
    (void)argument;
    return SQLITE_NOTFOUND;
}

static int file_sector_size(sqlite3_file *file)
{
    (void)file;
    return SECTOR_BYTES;
}

static int file_device_characteristics(sqlite3_file *file)
{
    (void)file;
    return 0;
}

static const sqlite3_io_methods file_methods = {
    .iVersion = 1,
    .xClose = file_close,
    .xRead = file_read,
    .xWrite = file_write,
    .xTruncate = file_truncate,
    .xSync = file_sync,
    .xFileSize = file_size,
    .xLock = file_lock,
    .xUnlock = file_lock,
    .xCheckReservedLock = file_check_reserved_lock,
    .xFileControl = file_control,
    .xSectorSize = file_sector_size,
    .xDeviceCharacteristics = file_device_characteristics,
};

static int vfs_open(sqlite3_vfs *vfs, sqlite3_filename name, sqlite3_file *file, int flags, int *opened_flags)
{
    (void)vfs;
    struct molecular_file *opened = (struct molecular_file *)file;
    opened->base.pMethods = NULL;
    if (name == NULL || name[0] != '/') {
        return SQLITE_CANTOPEN;
    }

    int access = O_RDONLY;
    if (flags & SQLITE_OPEN_READWRITE) {
        access = O_RDWR;
        if (flags & SQLITE_OPEN_CREATE) {
            access |= O_CREAT;
        }
        if (flags & SQLITE_OPEN_EXCLUSIVE) {
            access |= O_EXCL;
        }
    }

    int fd = open(name, access, FILE_MODE);
    if (fd < 0) {
        return SQLITE_CANTOPEN;
    }
    opened->fd = fd;
    opened->base.pMethods = &file_methods;
    if (opened_flags != NULL) {
        *opened_flags = flags;
    }
    return SQLITE_OK;
}

static int vfs_delete(sqlite3_vfs *vfs, const char *name, int sync_directory)
{
    (void)vfs;
    (void)sync_directory;
    if (unlink(name) == 0) {
        return SQLITE_OK;
    }
    return errno == ENOENT ? SQLITE_IOERR_DELETE_NOENT : SQLITE_IOERR_DELETE;
}

static int vfs_access(sqlite3_vfs *vfs, const char *name, int flags, int *result)
{
    (void)vfs;
    (void)flags;
    struct stat st;
    *result = stat(name, &st) == 0;
    return SQLITE_OK;
}

static int vfs_full_pathname(sqlite3_vfs *vfs, const char *name, int capacity, char *out)
{
    (void)vfs;
    size_t length = strlen(name);
    if (name[0] != '/' || length >= (size_t)capacity) {
        return SQLITE_CANTOPEN;
    }
    memcpy(out, name, length + 1);
    return SQLITE_OK;
}

static int vfs_randomness(sqlite3_vfs *vfs, int size, char *out)
{
    (void)vfs;
    int done = 0;
    while (done < size) {
        int chunk = size - done < ENTROPY_CHUNK_BYTES ? size - done : ENTROPY_CHUNK_BYTES;
        if (getentropy(out + done, (size_t)chunk) != 0) {
            break;
        }
        done += chunk;
    }
    return done;
}

static int vfs_sleep(sqlite3_vfs *vfs, int microseconds)
{
    (void)vfs;
    struct timespec delay = {
        .tv_sec = microseconds / US_PER_SEC,
        .tv_nsec = (long)(microseconds % US_PER_SEC) * NS_PER_US,
    };
    nanosleep(&delay, NULL);
    return microseconds;
}

static int vfs_current_time(sqlite3_vfs *vfs, sqlite3_int64 *julian_ms)
{
    (void)vfs;
    struct timespec now;
    if (clock_gettime(CLOCK_REALTIME, &now) != 0) {
        return SQLITE_ERROR;
    }
    *julian_ms = UNIX_EPOCH_JULIAN_MS + (sqlite3_int64)now.tv_sec * MS_PER_SEC + now.tv_nsec / NS_PER_MS;
    return SQLITE_OK;
}

static int vfs_last_error(sqlite3_vfs *vfs, int capacity, char *message)
{
    (void)vfs;
    (void)capacity;
    (void)message;
    return errno;
}

static sqlite3_vfs molecular_vfs = {
    .iVersion = 2,
    .szOsFile = sizeof(struct molecular_file),
    .mxPathname = PATH_MAX,
    .zName = "molecular",
    .xOpen = vfs_open,
    .xDelete = vfs_delete,
    .xAccess = vfs_access,
    .xFullPathname = vfs_full_pathname,
    .xRandomness = vfs_randomness,
    .xSleep = vfs_sleep,
    .xGetLastError = vfs_last_error,
    .xCurrentTimeInt64 = vfs_current_time,
};

int sqlite3_os_init(void)
{
    return sqlite3_vfs_register(&molecular_vfs, 1);
}

int sqlite3_os_end(void)
{
    return SQLITE_OK;
}

__attribute__((constructor)) static void sqlite_molecular_init(void)
{
    sqlite3_config(SQLITE_CONFIG_MUTEX, &molecular_mutex_methods);
    sqlite3_initialize();
}
