/*
 * Системный слой файлов библиотеки C поверх файлового корня c-library.
 *
 * Файл раскладывает ответы Rust-части в структуры заголовков: вариадический open, struct stat
 * для stat и fstat, поток каталога DIR и struct dirent. Значения O_*, SEEK_*, S_IF* и DT_*
 * Rust-часть получает отсюда константами.
 */

#include <dirent.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

const int c_library_o_accmode = O_ACCMODE;
const int c_library_o_rdonly = O_RDONLY;
const int c_library_o_wronly = O_WRONLY;
const int c_library_o_rdwr = O_RDWR;
const int c_library_o_creat = O_CREAT;
const int c_library_o_excl = O_EXCL;
const int c_library_o_trunc = O_TRUNC;
const int c_library_o_append = O_APPEND;

/* Флаги без действия в файловой модели: нет exec и терминалов, запись завершается до ответа. */
const int c_library_o_ignored = O_CLOEXEC | O_NOCTTY | O_NONBLOCK | O_SYNC;

const int c_library_seek_set = SEEK_SET;
const int c_library_seek_cur = SEEK_CUR;
const int c_library_seek_end = SEEK_END;

const uint32_t c_library_s_ifreg = S_IFREG;
const uint32_t c_library_s_ifdir = S_IFDIR;
const uint32_t c_library_s_ifchr = S_IFCHR;

const uint8_t c_library_dt_reg = DT_REG;
const uint8_t c_library_dt_dir = DT_DIR;

/* Метаданные узла. Раскладка совпадает с Node в files.rs. */
struct c_library_node {
    uint32_t mode;
    int64_t  size;
    uint64_t volume;
    uint64_t inode;
};

/* Поток каталога: DIR заголовка хранит текущую запись, listing - обход Rust-части. */
struct c_library_dir {
    DIR   dir;
    void *listing;
};

int   c_library_open(const char *path, int flags);
int   c_library_stat(const char *path, struct c_library_node *node);
int   c_library_fstat(int fd, struct c_library_node *node);
void *c_library_opendir(const char *path);
int   c_library_readdir(void *listing, char *name, size_t capacity, uint8_t *type);
void  c_library_closedir(void *listing);

/* Узлы не хранят прав, и режим создаваемого файла не читается. */
int open(const char *path, int flags, ...)
{
    return c_library_open(path, flags);
}

static void fill_stat(struct stat *st, const struct c_library_node *node)
{
    memset(st, 0, sizeof *st);
    st->st_mode = node->mode;
    st->st_size = node->size;
    st->st_dev = node->volume;
    st->st_ino = node->inode;
    st->st_nlink = 1;
}

int stat(const char *restrict path, struct stat *restrict st)
{
    struct c_library_node node;
    if (c_library_stat(path, &node) != 0) {
        return -1;
    }
    fill_stat(st, &node);
    return 0;
}

int fstat(int fd, struct stat *st)
{
    struct c_library_node node;
    if (c_library_fstat(fd, &node) != 0) {
        return -1;
    }
    fill_stat(st, &node);
    return 0;
}

DIR *opendir(const char *path)
{
    struct c_library_dir *stream = malloc(sizeof *stream);
    if (stream == NULL) {
        return NULL;
    }

    void *listing = c_library_opendir(path);
    if (listing == NULL) {
        free(stream);
        return NULL;
    }

    *stream = (struct c_library_dir){ .listing = listing };
    return &stream->dir;
}

struct dirent *readdir(DIR *dir)
{
    struct c_library_dir *stream = (struct c_library_dir *)dir;
    struct dirent        *entry = &dir->dirent;
    if (c_library_readdir(stream->listing, entry->d_name, sizeof entry->d_name, &entry->d_type) != 1) {
        return NULL;
    }
    entry->d_ino = 0;
    return entry;
}

int closedir(DIR *dir)
{
    struct c_library_dir *stream = (struct c_library_dir *)dir;
    c_library_closedir(stream->listing);
    free(stream);
    return 0;
}
