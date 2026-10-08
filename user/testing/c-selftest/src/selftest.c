#define _POSIX_C_SOURCE 200809L

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <stdalign.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

/* Код выхода после всех проверок: отличим от успеха `main` и от провала. */
#define PASSED_EXIT_CODE 3

/* Запрос сверх запаса бюджета, который выдаёт стенду родитель. */
#define OVER_BUDGET_BYTES (64u * 1024u * 1024u)

/* Размер, который куча процесса выделяет мимо классов размеров. */
#define LARGE_BYTES (128u * 1024u)

/* Страница, до которой округляется размер стека потока. */
#define PAGE_BYTES 4096u

/* Значение выхода потока, которое выдаёт `pthread_join`. */
#define EXIT_VALUE ((void *)0x5EED)

/* Серия отсоединённых потоков: запас кучи стенда вмещает три таких стека, а вся серия - нет. */
#define DETACHED_STACK_BYTES (1024u * 1024u)
#define DETACHED_THREADS     8

/* Конкурентный вывод: testrunner ждёт строки `thread T line L` для каждой пары. */
#define PRINTING_THREADS 4
#define PRINTED_LINES    8

/* Потоки и шаги проверок синхронизации. */
#define SYNC_THREADS 4
#define SYNC_ROUNDS  200

/* Файлы из потоков: серия блоков байта потока и строки общего потока `FILE`. */
#define FILE_BLOCKS      8
#define FILE_BLOCK_BYTES 512u
#define FILE_LINES       32

_Static_assert(PTHREAD_KEYS_MAX >= _POSIX_THREAD_KEYS_MAX, "pthread.h offers the POSIX minimum of keys");

/* Каталог файловых проверок в памяти и его соседний каталог. */
#define SCRATCH "/tmp/c-selftest"
#define SCRATCH_PEER "/tmp/c-selftest-peer"

/* Файл образа testrunner и его содержимое. */
#define IMAGE_FILE "/boot/testrunner/sample"
#define IMAGE_FILE_BODY "hello from the userland image\n"

/* Отказ файловой системы: имя окна отказывающего сервера и ожидаемый errno. */
struct refusal {
    const char *name;
    int         error;
};

static int failures;

static int keys_created;

static _Atomic(pthread_t) published;

static atomic_int detached_returned;

static pthread_key_t value_key;

static pthread_key_t destructor_key;

/* Что видел деструктор ключа в завершающемся потоке. */
static pthread_t   body_self;
static int         destructor_saw_body;
static int         destructor_saw_errno;
static void *volatile destructor_saw_value;

static pthread_mutex_t counter_mutex = PTHREAD_MUTEX_INITIALIZER;
static int             counter;

/* Одноместный буфер производителя и потребителя. */
static pthread_mutex_t slot_mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t  slot_filled = PTHREAD_COND_INITIALIZER;
static pthread_cond_t  slot_emptied = PTHREAD_COND_INITIALIZER;
static int             slot;

/* Ждущие уведомления всех: число вставших в ожидание и разрешение выйти. */
static pthread_mutex_t gate_mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t  gate_changed = PTHREAD_COND_INITIALIZER;
static int             gate_waiters;
static int             gate_open;

static pthread_once_t once = PTHREAD_ONCE_INIT;
static atomic_int     once_calls;
static int            once_result;

static FILE *shared_stream;

static const struct refusal refusals[] = {
    { "NotFound", ENOENT },
    { "NotADirectory", ENOTDIR },
    { "IsADirectory", EISDIR },
    { "NotEmpty", ENOTEMPTY },
    { "AlreadyExists", EEXIST },
    { "InvalidPath", EINVAL },
    { "BadDescriptor", EIO },
    { "TooManyOpenFiles", EMFILE },
    { "ReadOnly", EROFS },
    { "NoSpace", ENOSPC },
    { "CrossMount", EXDEV },
    { "NoSession", EIO },
    { "NoBuffer", EIO },
    { "BufferTooSmall", EIO },
    { "NotSupported", ENOTSUP },
    { "Unresponsive", ETIMEDOUT },
    { "WrongMode", EBADF },
};

/* Результат и размеры идут через volatile: компилятор не сворачивает вызовы библиотеки. */
static void *volatile kept;
static volatile size_t over_budget = OVER_BUDGET_BYTES;
static volatile size_t unaligned_offset = 1;
static volatile size_t copied_len = 37;

static void check(int passed, const char *what)
{
    if (!passed) {
        fprintf(stderr, "check failed: %s\n", what);
        failures++;
    }
}

/* Проверка под экспортируемым именем. */
void selftest_check(int passed, const char *what)
{
    check(passed, what);
}

static int fails_with(int result, int error)
{
    return result == -1 && errno == error;
}

static int aligned_to(const void *pointer, size_t align)
{
    return ((uintptr_t)pointer & (align - 1)) == 0;
}

static int filled_with(const unsigned char *bytes, size_t len, unsigned char value)
{
    for (size_t i = 0; i < len; i++) {
        if (bytes[i] != value) {
            return 0;
        }
    }
    return 1;
}

static void check_arguments(int argc, char **argv)
{
    check(argc == 1, "argc is 1");
    check(argv[0] != NULL && strcmp(argv[0], "c-selftest") == 0, "argv[0] names the program");
    check(argv[1] == NULL, "argv ends with NULL");
    argv[0][0] = 'C';
    check(argv[0][0] == 'C', "argv[0] is writable");
}

static void check_descriptors(void)
{
    char byte = 'x';

    errno = 0;
    check(write(7, &byte, 1) == -1 && errno == EBADF, "write to fd 7 fails with EBADF");

    check(read(0, &byte, 1) == 0, "read from fd 0 reaches end of data");

    errno = 0;
    check(lseek(1, 0, SEEK_CUR) == -1 && errno == ESPIPE, "lseek on fd 1 fails with ESPIPE");
}

static void check_allocation(void)
{
    unsigned char *block = malloc(24);
    check(block != NULL && aligned_to(block, alignof(max_align_t)), "malloc aligns the block");
    memset(block, 0xA5, 24);

    block = realloc(block, 4096);
    check(block != NULL && filled_with(block, 24, 0xA5), "realloc keeps the contents");
    memset(block, 0x5A, 4096);

    block = realloc(block, LARGE_BYTES);
    check(block != NULL && filled_with(block, 4096, 0x5A), "realloc to a large block keeps the contents");

    block = realloc(block, 16);
    check(block != NULL && filled_with(block, 16, 0x5A), "realloc to a smaller block keeps the head");
    free(block);

    unsigned char *zeroed = calloc(32, 4);
    check(zeroed != NULL && filled_with(zeroed, 128, 0), "calloc zeroes the block");
    free(zeroed);

    void *page = aligned_alloc(4096, 100);
    check(page != NULL && aligned_to(page, 4096), "aligned_alloc honours the alignment");
    free(page);

    void *line = NULL;
    check(posix_memalign(&line, 64, 40) == 0 && aligned_to(line, 64), "posix_memalign honours the alignment");
    free(line);

    void *untouched = NULL;
    check(posix_memalign(&untouched, 3, 8) == EINVAL && untouched == NULL, "posix_memalign rejects a non-power-of-two");

    free(NULL);

    errno = 0;
    kept = malloc(over_budget);
    check(kept == NULL && errno == ENOMEM, "malloc over the budget fails with ENOMEM");
}

static void check_unaligned_buffers(void)
{
    alignas(16) unsigned char source[64];
    alignas(16) unsigned char target[64];
    for (size_t i = 0; i < sizeof source; i++) {
        source[i] = (unsigned char)i;
    }
    memset(target, 0, sizeof target);

    size_t offset = unaligned_offset;
    size_t len = copied_len;
    memcpy(target + offset, source + offset + 2, len);
    int copied = 1;
    for (size_t i = 0; i < len; i++) {
        copied &= target[offset + i] == source[offset + 2 + i];
    }
    check(copied && target[0] == 0 && target[offset + len] == 0, "memcpy copies between unaligned buffers");

    char *text = (char *)target + offset;
    int printed = snprintf(text, sizeof target - offset, "%d %s %lx %.2f", -42, "word", 0xbeefUL, 1.5);
    check(printed == 18 && strcmp(text, "-42 word beef 1.50") == 0, "snprintf formats into an unaligned buffer");
}

void selftest_before_root(void)
{
    errno = 0;
    check(fails_with(open(SCRATCH "/early", O_RDONLY), ENOTCONN), "open before the root fails with ENOTCONN");
}

static int file_holds(const char *path, const char *body)
{
    char buffer[64];
    int  fd = open(path, O_RDONLY);
    if (fd < 0) {
        return 0;
    }
    ssize_t count = read(fd, buffer, sizeof buffer);
    close(fd);
    return count == (ssize_t)strlen(body) && memcmp(buffer, body, (size_t)count) == 0;
}

static void check_open(void)
{
    struct stat st;

    int first = open(SCRATCH "/first", O_RDWR | O_CREAT | O_CLOEXEC, 0644);
    check(first >= 3 && stat(SCRATCH "/first", &st) == 0, "open with O_CREAT creates the file");

    int second = open(SCRATCH "/second", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    close(first);
    int reused = open(SCRATCH "/second", O_RDONLY);
    check(first == 3 && reused == 3, "open takes the lowest free descriptor");
    close(reused);
    close(second);

    errno = 0;
    check(fails_with(open(SCRATCH "/first", O_RDWR | O_CREAT | O_EXCL, 0644), EEXIST),
          "O_EXCL refuses an existing file with EEXIST");

    errno = 0;
    check(fails_with(open(SCRATCH "/missing", O_RDONLY), ENOENT), "open of a missing file fails with ENOENT");

    errno = 0;
    check(fails_with(open("data.bin", O_RDONLY), EINVAL), "a relative path fails with EINVAL");

    errno = 0;
    check(fails_with(open(SCRATCH, O_RDONLY | O_DIRECTORY), EINVAL), "O_DIRECTORY fails with EINVAL");
}

static void check_read_write(void)
{
    char buffer[16];
    int  fd = open(SCRATCH "/data", O_RDWR | O_CREAT, 0644);
    check(write(fd, "hello", 5) == 5, "write puts the bytes into the file");
    check(lseek(fd, 0, SEEK_SET) == 0, "lseek rewinds the descriptor");
    check(read(fd, buffer, sizeof buffer) == 5 && memcmp(buffer, "hello", 5) == 0, "read returns the written bytes");
    check(read(fd, buffer, sizeof buffer) == 0, "read at the end returns 0");
    close(fd);

    struct stat st;
    check(stat(SCRATCH "/data", &st) == 0 && S_ISREG(st.st_mode) && st.st_size == 5, "stat reports a 5-byte file");

    fd = open(SCRATCH "/data", O_WRONLY | O_APPEND);
    check(write(fd, " world", 6) == 6, "write with O_APPEND succeeds");
    check(lseek(fd, 0, SEEK_SET) == 0, "lseek rewinds an O_APPEND descriptor");
    check(write(fd, "", 0) == 0 && lseek(fd, 0, SEEK_CUR) == 0, "an empty O_APPEND write keeps the position");
    close(fd);
    check(file_holds(SCRATCH "/data", "hello world"), "O_APPEND writes after the old contents");

    fd = open(SCRATCH "/data", O_RDONLY);
    check(fstat(fd, &st) == 0 && S_ISREG(st.st_mode) && st.st_size == 11, "fstat reports the appended size");
    errno = 0;
    check(fails_with((int)write(fd, "x", 1), EBADF), "write to an O_RDONLY descriptor fails with EBADF");
    close(fd);

    check(fstat(1, &st) == 0 && S_ISCHR(st.st_mode), "fstat on fd 1 reports a character device");
}

static void check_streams(void)
{
    char  buffer[32] = { 0 };
    FILE *stream = fopen(SCRATCH "/stream", "w");
    check(stream != NULL && fwrite("buffered text", 1, 13, stream) == 13 && fclose(stream) == 0, "fwrite through fopen succeeds");

    stream = fopen(SCRATCH "/stream", "r");
    check(stream != NULL && fread(buffer, 1, sizeof buffer, stream) == 13 && strcmp(buffer, "buffered text") == 0,
          "fread returns the bytes fwrite wrote");
    if (stream != NULL) {
        fclose(stream);
    }
    check(remove(SCRATCH "/stream") == 0, "remove deletes the file");
}

static void check_paths(void)
{
    struct stat st;

    check(mkdir(SCRATCH "/made", 0755) == 0 && rmdir(SCRATCH "/made") == 0, "mkdir and rmdir succeed");
    errno = 0;
    check(fails_with(stat(SCRATCH "/made", &st), ENOENT), "stat of a removed directory fails with ENOENT");

    errno = 0;
    check(fails_with(mkdir(SCRATCH, 0755), EEXIST), "mkdir of an existing name fails with EEXIST");

    errno = 0;
    check(fails_with(rmdir(SCRATCH), ENOTEMPTY), "rmdir of a non-empty directory fails with ENOTEMPTY");

    check(rename(SCRATCH "/first", SCRATCH "/renamed") == 0 && stat(SCRATCH "/renamed", &st) == 0,
          "rename moves a file within /tmp");
    errno = 0;
    check(fails_with(rename(SCRATCH "/renamed", "/boot/renamed"), EXDEV), "rename across mounts fails with EXDEV");

    check(file_holds(IMAGE_FILE, IMAGE_FILE_BODY), "an image file reads");
    errno = 0;
    check(fails_with(open(IMAGE_FILE, O_WRONLY), EROFS), "opening an image file for writing fails with EROFS");
}

static void check_directory(void)
{
    close(open(SCRATCH_PEER "/a", O_WRONLY | O_CREAT, 0644));
    mkdir(SCRATCH_PEER "/b", 0755);

    int  file_seen = 0;
    int  directory_seen = 0;
    int  others = 0;
    DIR *dir = opendir(SCRATCH_PEER);
    check(dir != NULL, "opendir opens a directory");
    if (dir == NULL) {
        return;
    }
    for (struct dirent *entry = readdir(dir); entry != NULL; entry = readdir(dir)) {
        if (strcmp(entry->d_name, "a") == 0 && entry->d_type == DT_REG) {
            file_seen++;
        } else if (strcmp(entry->d_name, "b") == 0 && entry->d_type == DT_DIR) {
            directory_seen++;
        } else {
            others++;
        }
    }
    check(closedir(dir) == 0, "closedir releases the stream");
    check(file_seen == 1 && directory_seen == 1 && others == 0, "readdir returns the file and the directory once");
}

static void check_refusals(void)
{
    char path[64];
    for (size_t i = 0; i < sizeof refusals / sizeof refusals[0]; i++) {
        snprintf(path, sizeof path, "/failing/%s/file", refusals[i].name);
        errno = 0;
        check(fails_with(open(path, O_RDONLY), refusals[i].error), refusals[i].name);
    }
}

static void check_files(void)
{
    check(mkdir(SCRATCH, 0755) == 0 && mkdir(SCRATCH_PEER, 0755) == 0, "mkdir creates the scratch directories");
    check_open();
    check_read_write();
    check_streams();
    check_paths();
    check_directory();
    check_refusals();
}

static void *return_argument(void *arg)
{
    return arg;
}

static void exit_from_nested_call(void *value)
{
    pthread_exit(value);
}

static void *exit_nested(void *arg)
{
    exit_from_nested_call(arg);
    return NULL;
}

static void *join_self(void *arg)
{
    (void)arg;
    return (void *)(intptr_t)pthread_join(pthread_self(), NULL);
}

static void *compare_published(void *arg)
{
    (void)arg;
    pthread_t id;
    while ((id = atomic_load(&published)) == NULL) {
    }
    return (void *)(intptr_t)pthread_equal(pthread_self(), id);
}

static void *mark_returned(void *arg)
{
    (void)arg;
    atomic_store(&detached_returned, 1);
    return NULL;
}

static void *keep_own_value(void *arg)
{
    int started_empty = pthread_getspecific(value_key) == NULL;
    pthread_setspecific(value_key, arg);
    return (void *)(intptr_t)(started_empty && pthread_getspecific(value_key) == arg);
}

static void record_destructor(void *value)
{
    destructor_saw_value = value;
    destructor_saw_body = pthread_equal(pthread_self(), body_self);
    destructor_saw_errno = errno;
}

static void *fail_with_destructor(void *arg)
{
    body_self = pthread_self();
    pthread_setspecific(destructor_key, arg);
    char byte = 'x';
    write(7, &byte, 1);
    return (void *)(intptr_t)(errno == EBADF);
}

static void *print_lines(void *arg)
{
    int thread = (int)(intptr_t)arg;
    for (int line = 0; line < PRINTED_LINES; line++) {
        printf("thread %d line %d\n", thread, line);
    }
    return NULL;
}

static int joined_with(pthread_t thread, void *expected)
{
    void *value = NULL;
    return pthread_join(thread, &value) == 0 && value == expected;
}

static void check_thread_lifecycle(void)
{
    pthread_t thread;

    check(pthread_create(&thread, NULL, return_argument, EXIT_VALUE) == 0 && joined_with(thread, EXIT_VALUE),
          "pthread_join returns the value of the thread function");

    check(pthread_create(&thread, NULL, exit_nested, EXIT_VALUE) == 0 && joined_with(thread, EXIT_VALUE),
          "pthread_join returns the value of a nested pthread_exit");

    check(pthread_create(&thread, NULL, join_self, NULL) == 0 && joined_with(thread, (void *)(intptr_t)EDEADLK),
          "pthread_join of the calling thread fails with EDEADLK");

    check(pthread_equal(pthread_self(), pthread_self()), "pthread_equal matches the calling thread");
    atomic_store(&published, NULL);
    int created = pthread_create(&thread, NULL, compare_published, NULL) == 0;
    if (created) {
        atomic_store(&published, thread);
    }
    check(created && !pthread_equal(thread, pthread_self()) && joined_with(thread, (void *)1),
          "pthread_self of a thread equals the identifier from pthread_create");
}

static void check_thread_attributes(void)
{
    pthread_attr_t attr;
    size_t         stack_bytes = 0;
    int            detach_state = -1;

    check(pthread_attr_init(&attr) == 0 && pthread_attr_getstacksize(&attr, &stack_bytes) == 0 &&
              stack_bytes >= PTHREAD_STACK_MIN,
          "pthread_attr_init sets a default stack size");
    check(pthread_attr_getdetachstate(&attr, &detach_state) == 0 && detach_state == PTHREAD_CREATE_JOINABLE,
          "pthread_attr_init makes threads joinable");

    check(pthread_attr_setstacksize(&attr, PTHREAD_STACK_MIN - 1) == EINVAL,
          "pthread_attr_setstacksize below PTHREAD_STACK_MIN fails with EINVAL");
    check(pthread_attr_setstacksize(&attr, PTHREAD_STACK_MIN + 1) == 0 &&
              pthread_attr_getstacksize(&attr, &stack_bytes) == 0 && stack_bytes == PTHREAD_STACK_MIN + PAGE_BYTES,
          "pthread_attr_setstacksize rounds up to a page");
    check(pthread_attr_setdetachstate(&attr, 7) == EINVAL, "pthread_attr_setdetachstate rejects an unknown state");

    pthread_t thread;
    check(pthread_attr_setstacksize(&attr, over_budget) == 0 &&
              pthread_create(&thread, &attr, return_argument, NULL) == EAGAIN,
          "pthread_create over the budget fails with EAGAIN");
    pthread_attr_destroy(&attr);
}

static void check_detached_threads(void)
{
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, DETACHED_STACK_BYTES);

    int created = 0;
    for (int i = 0; i < DETACHED_THREADS; i++) {
        /* Чётные потоки отсоединены атрибутом, нечётные - вызовом `pthread_detach`. */
        int       by_attribute = i % 2 == 0;
        pthread_t thread;
        pthread_attr_setdetachstate(&attr, by_attribute ? PTHREAD_CREATE_DETACHED : PTHREAD_CREATE_JOINABLE);
        atomic_store(&detached_returned, 0);
        if (pthread_create(&thread, &attr, mark_returned, NULL) != 0) {
            break;
        }
        if (!by_attribute && pthread_detach(thread) != 0) {
            break;
        }
        created++;
        while (!atomic_load(&detached_returned)) {
        }
    }
    pthread_attr_destroy(&attr);
    check(created == DETACHED_THREADS, "detached threads release their stacks");
}

static void check_thread_keys(void)
{
    pthread_t thread;
    void     *joined = NULL;

    int created = pthread_key_create(&value_key, NULL) == 0;
    keys_created += created;
    check(created && pthread_getspecific(value_key) == NULL, "a new key starts with NULL");
    pthread_setspecific(value_key, EXIT_VALUE);
    check(pthread_create(&thread, NULL, keep_own_value, &value_key) == 0 && joined_with(thread, (void *)1) &&
              pthread_getspecific(value_key) == EXIT_VALUE,
          "each thread reads its own value of a key");

    created = pthread_key_create(&destructor_key, record_destructor) == 0;
    keys_created += created;
    errno = ERANGE;
    check(created && pthread_create(&thread, NULL, fail_with_destructor, EXIT_VALUE) == 0 &&
              pthread_join(thread, &joined) == 0 && joined == (void *)1,
          "a thread reads errno of its own failure");
    check(errno == ERANGE, "a failure in another thread keeps errno");
    check(destructor_saw_value == EXIT_VALUE && destructor_saw_body && destructor_saw_errno == EBADF,
          "a key destructor runs in the exiting thread");
}

static void check_concurrent_output(void)
{
    pthread_t threads[PRINTING_THREADS];
    int       created = 0;
    for (int i = 0; i < PRINTING_THREADS; i++) {
        if (pthread_create(&threads[i], NULL, print_lines, (void *)(intptr_t)i) != 0) {
            break;
        }
        created++;
    }
    int joined = 0;
    for (int i = 0; i < created; i++) {
        joined += joined_with(threads[i], NULL);
    }
    fflush(stdout);
    check(created == PRINTING_THREADS && joined == created, "printing threads run to the end");
}

static void *count_under_mutex(void *arg)
{
    (void)arg;

    for (int i = 0; i < SYNC_ROUNDS; i++) {
        pthread_mutex_lock(&counter_mutex);
        counter++;
        pthread_mutex_unlock(&counter_mutex);
    }

    return NULL;
}

/* Результат `pthread_mutex_unlock` из чужого потока. */
static void *unlock_foreign(void *arg)
{
    return (void *)(intptr_t)pthread_mutex_unlock(arg);
}

/* Захват без ожидания из чужого потока с освобождением захваченного. */
static void *trylock_foreign(void *arg)
{
    int result = pthread_mutex_trylock(arg);

    if (result == 0) {
        pthread_mutex_unlock(arg);
    }

    return (void *)(intptr_t)result;
}

static int foreign(void *(*body)(void *), pthread_mutex_t *mutex, int expected)
{
    pthread_t thread;

    return pthread_create(&thread, NULL, body, mutex) == 0 && joined_with(thread, (void *)(intptr_t)expected);
}

static int init_kind(pthread_mutex_t *mutex, int kind)
{
    pthread_mutexattr_t attr;
    int                 ready = pthread_mutexattr_init(&attr) == 0 && pthread_mutexattr_settype(&attr, kind) == 0 &&
                                pthread_mutex_init(mutex, &attr) == 0;

    pthread_mutexattr_destroy(&attr);

    return ready;
}

static void check_mutexes(void)
{
    pthread_t threads[SYNC_THREADS];
    int       created = 0;

    for (int i = 0; i < SYNC_THREADS && pthread_create(&threads[i], NULL, count_under_mutex, NULL) == 0; i++) {
        created++;
    }

    for (int i = 0; i < created; i++) {
        joined_with(threads[i], NULL);
    }

    check(created == SYNC_THREADS && counter == SYNC_THREADS * SYNC_ROUNDS, "threads count under a static mutex");

    pthread_mutex_t     mutex;
    pthread_cond_t      unused = PTHREAD_COND_INITIALIZER;
    pthread_mutexattr_t attr;

    pthread_mutexattr_init(&attr);
    check(pthread_mutexattr_settype(&attr, 7) == EINVAL, "pthread_mutexattr_settype rejects an unknown kind");
    pthread_mutexattr_destroy(&attr);

    check(init_kind(&mutex, PTHREAD_MUTEX_NORMAL) && pthread_mutex_lock(&mutex) == 0 &&
              pthread_mutex_trylock(&mutex) == EBUSY && pthread_mutex_unlock(&mutex) == 0,
          "pthread_mutex_trylock of a locked mutex fails with EBUSY");

    check(init_kind(&mutex, PTHREAD_MUTEX_ERRORCHECK) && pthread_mutex_lock(&mutex) == 0 &&
              pthread_mutex_lock(&mutex) == EDEADLK,
          "an error-checking mutex refuses relocking with EDEADLK");
    check(foreign(unlock_foreign, &mutex, EPERM) && pthread_mutex_unlock(&mutex) == 0,
          "an error-checking mutex refuses a foreign unlock with EPERM");
    check(pthread_cond_wait(&unused, &mutex) == EPERM, "pthread_cond_wait without the mutex fails with EPERM");
    pthread_mutex_destroy(&mutex);

    check(init_kind(&mutex, PTHREAD_MUTEX_RECURSIVE) && pthread_mutex_lock(&mutex) == 0 &&
              pthread_mutex_trylock(&mutex) == 0 && foreign(trylock_foreign, &mutex, EBUSY) &&
              foreign(unlock_foreign, &mutex, EPERM) && pthread_mutex_unlock(&mutex) == 0 &&
              pthread_mutex_unlock(&mutex) == 0 && foreign(trylock_foreign, &mutex, 0),
          "a recursive mutex is free after as many unlocks as locks");
    pthread_mutex_destroy(&mutex);
}

static void *produce(void *arg)
{
    (void)arg;

    for (int value = 1; value <= SYNC_ROUNDS; value++) {
        pthread_mutex_lock(&slot_mutex);
        while (slot != 0) {
            pthread_cond_wait(&slot_emptied, &slot_mutex);
        }

        slot = value;
        pthread_cond_signal(&slot_filled);
        pthread_mutex_unlock(&slot_mutex);
    }

    return NULL;
}

static void *wait_for_gate(void *arg)
{
    (void)arg;

    pthread_mutex_lock(&gate_mutex);
    gate_waiters++;
    pthread_cond_broadcast(&gate_changed);
    while (!gate_open) {
        pthread_cond_wait(&gate_changed, &gate_mutex);
    }

    pthread_mutex_unlock(&gate_mutex);

    return NULL;
}

static void check_conditions(void)
{
    pthread_t producer;
    int       received = 0;
    int       created = pthread_create(&producer, NULL, produce, NULL) == 0;

    for (int expected = 1; created && expected <= SYNC_ROUNDS; expected++) {
        pthread_mutex_lock(&slot_mutex);
        while (slot == 0) {
            pthread_cond_wait(&slot_filled, &slot_mutex);
        }

        received += slot == expected;
        slot = 0;
        pthread_mutex_unlock(&slot_mutex);
        pthread_cond_signal(&slot_emptied);
    }

    check(created && joined_with(producer, NULL) && received == SYNC_ROUNDS,
          "the consumer receives every value after pthread_cond_signal");

    pthread_t waiters[SYNC_THREADS];
    int       started = 0;

    for (int i = 0; i < SYNC_THREADS && pthread_create(&waiters[i], NULL, wait_for_gate, NULL) == 0; i++) {
        started++;
    }

    pthread_mutex_lock(&gate_mutex);
    while (gate_waiters < started) {
        pthread_cond_wait(&gate_changed, &gate_mutex);
    }

    gate_open = 1;
    pthread_cond_broadcast(&gate_changed);
    pthread_mutex_unlock(&gate_mutex);
    int joined = 0;

    for (int i = 0; i < started; i++) {
        joined += joined_with(waiters[i], NULL);
    }

    check(started == SYNC_THREADS && joined == started, "pthread_cond_broadcast releases every waiter");
}

static void initialize_once(void)
{
    atomic_fetch_add(&once_calls, 1);
    for (volatile int i = 0; i < SYNC_ROUNDS * SYNC_ROUNDS; i++) {
    }

    once_result = 1;
}

static void *call_once(void *arg)
{
    (void)arg;

    pthread_once(&once, initialize_once);

    return (void *)(intptr_t)once_result;
}

static void check_once(void)
{
    pthread_t threads[SYNC_THREADS];
    int       created = 0;

    for (int i = 0; i < SYNC_THREADS && pthread_create(&threads[i], NULL, call_once, NULL) == 0; i++) {
        created++;
    }

    int saw_result = 0;

    for (int i = 0; i < created; i++) {
        saw_result += joined_with(threads[i], (void *)1);
    }

    check(created == SYNC_THREADS && saw_result == created && atomic_load(&once_calls) == 1,
          "pthread_once runs the function once for concurrent callers");
}

static void thread_file_path(char *path, size_t len, int thread)
{
    snprintf(path, len, SCRATCH "/thread-%d", thread);
}

/* Поток пишет серию блоков своего байта в свой файл и читает их обратно. */
static void *write_own_file(void *arg)
{
    int           thread = (int)(intptr_t)arg;
    unsigned char block[FILE_BLOCK_BYTES];
    char          path[64];

    thread_file_path(path, sizeof path, thread);
    int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);

    if (fd < 0) {
        return NULL;
    }

    int intact = 1;

    memset(block, 'a' + thread, sizeof block);
    for (int i = 0; i < FILE_BLOCKS; i++) {
        intact &= write(fd, block, sizeof block) == (ssize_t)sizeof block;
    }

    intact &= lseek(fd, 0, SEEK_SET) == 0;
    for (int i = 0; i < FILE_BLOCKS; i++) {
        memset(block, 0, sizeof block);
        intact &= read(fd, block, sizeof block) == (ssize_t)sizeof block && filled_with(block, sizeof block, 'a' + thread);
    }

    intact &= close(fd) == 0;

    return (void *)(intptr_t)intact;
}

static void *print_to_shared(void *arg)
{
    int thread = (int)(intptr_t)arg;

    for (int line = 0; line < FILE_LINES; line++) {
        fprintf(shared_stream, "%d %d\n", thread, line);
    }

    return NULL;
}

static void check_thread_files(void)
{
    pthread_t threads[SYNC_THREADS];
    int       created = 0;

    for (int i = 0; i < SYNC_THREADS && pthread_create(&threads[i], NULL, write_own_file, (void *)(intptr_t)i) == 0;
         i++) {
        created++;
    }

    int intact = 0;

    for (int i = 0; i < created; i++) {
        struct stat st;
        char        path[64];

        thread_file_path(path, sizeof path, i);
        intact += joined_with(threads[i], (void *)1) && stat(path, &st) == 0 &&
                  st.st_size == FILE_BLOCKS * FILE_BLOCK_BYTES;
    }

    check(created == SYNC_THREADS && intact == created, "each thread reads back its own file");

    shared_stream = fopen(SCRATCH "/shared", "w");
    created = 0;
    for (int i = 0; shared_stream != NULL && i < SYNC_THREADS &&
                    pthread_create(&threads[i], NULL, print_to_shared, (void *)(intptr_t)i) == 0;
         i++) {
        created++;
    }

    for (int i = 0; i < created; i++) {
        joined_with(threads[i], NULL);
    }

    int closed = shared_stream != NULL && fclose(shared_stream) == 0;

    int   next[SYNC_THREADS] = { 0 };
    int   lines = 0;
    int   ordered = 1;
    int   thread = 0;
    int   line = 0;
    FILE *stream = fopen(SCRATCH "/shared", "r");

    while (stream != NULL && fscanf(stream, "%d %d\n", &thread, &line) == 2) {
        ordered &= thread >= 0 && thread < SYNC_THREADS && line == next[thread];

        if (thread >= 0 && thread < SYNC_THREADS) {
            next[thread]++;
        }

        lines++;
    }

    if (stream != NULL) {
        ordered &= feof(stream) != 0;
        fclose(stream);
    }

    check(created == SYNC_THREADS && closed && ordered && lines == SYNC_THREADS * FILE_LINES,
          "threads print whole lines in order to a shared FILE");
}

static void check_key_exhaustion(void)
{
    pthread_key_t key = 0;
    pthread_key_t first = 0;
    int           result = 0;
    while (keys_created <= PTHREAD_KEYS_MAX && (result = pthread_key_create(&key, NULL)) == 0) {
        if (first == 0) {
            first = key;
        }
        keys_created++;
    }
    check(result == EAGAIN && keys_created == PTHREAD_KEYS_MAX, "PTHREAD_KEYS_MAX keys exhaust the keys");

    check(pthread_key_delete(first) == 0 && pthread_key_create(&key, NULL) == EAGAIN, "a deleted key is not reused");
}

static void check_threads(void)
{
    check_thread_lifecycle();
    check_thread_attributes();
    check_detached_threads();
    check_thread_keys();
    check_concurrent_output();
    check_mutexes();
    check_conditions();
    check_once();
    check_thread_files();
    check_key_exhaustion();
}

static void write_marker(const char *marker)
{
    write(2, marker, strlen(marker));
}

static void marker_a(void)
{
    write_marker("atexit A\n");
}

static void marker_b(void)
{
    write_marker("atexit B\n");
}

int selftest_main(int argc, char **argv)
{
    check_arguments(argc, argv);
    check_descriptors();
    check_allocation();
    check_unaligned_buffers();
    check_files();
    check_threads();
    if (failures != 0) {
        exit(1);
    }

    atexit(marker_a);
    atexit(marker_b);
    printf("exit without newline");
    exit(PASSED_EXIT_CODE);
}
