/*
 * Сценарии спецификации sqlite над базой на `/tmp`: режим сборки, мьютексы, относительный путь,
 * повторное открытие, откат и фиксация без журнала, база только для чтения и вставки из
 * нескольких потоков через одно соединение.
 *
 * Проверки сверяют строки выборок с ожидаемым текстом: столбцы через ` | `, по строке текста на
 * запись. `trace` пишет в `stdout` запросы sqlite и их результаты одной строкой на вызов.
 */

#include <errno.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "sqlite3.h"

#define DATABASE "/tmp/sqlite-selftest.db"
#define JOURNAL  DATABASE "-journal"

#define READ_WRITE (SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE)

/* Вставки потоков через одно соединение. */
#define INSERTING_THREADS 4
#define THREAD_INSERTS    100

#define TRACE_LINE_BYTES 256
#define ROWS_BYTES       512

#define BOOKS_BY_YEAR "SELECT title, author, year FROM books ORDER BY year"

#define SHELF                                                                                      \
    "Solaris | Stanisław Lem | 1961\n"                                                             \
    "A Wizard of Earthsea | Ursula K. Le Guin | 1968\n"                                            \
    "The Dispossessed | Ursula K. Le Guin | 1974\n"

struct rows {
    char   text[ROWS_BYTES];
    size_t used;
    int    overflow;
};

static int failures;

static atomic_int unfinished_inserts;

void selftest_check(int passed, const char *what)
{
    if (!passed) {
        fprintf(stderr, "sqlite-selftest: FAILED %s\n", what);
        failures++;
    }
}

static void trace(const char *format, ...)
{
    char    line[TRACE_LINE_BYTES];
    va_list arguments;
    va_start(arguments, format);
    vsnprintf(line, sizeof line, format, arguments);
    va_end(arguments);
    puts(line);
}

static const char *outcome(int result, const char *message)
{
    return result == SQLITE_OK ? "ok" : message;
}

static sqlite3 *open_database(int flags)
{
    sqlite3 *db = NULL;
    int      result = sqlite3_open_v2(DATABASE, &db, flags, NULL);
    trace("open %s flags 0x%x -> %s", DATABASE, flags, outcome(result, sqlite3_errstr(result)));
    if (result != SQLITE_OK) {
        sqlite3_close(db);
        return NULL;
    }
    return db;
}

static int run(sqlite3 *db, const char *sql)
{
    int result = sqlite3_exec(db, sql, NULL, NULL, NULL);
    trace("exec %s -> %s", sql, outcome(result, sqlite3_errmsg(db)));
    return result;
}

static void append(struct rows *rows, const char *part)
{
    size_t length = strlen(part);
    if (rows->used + length >= sizeof rows->text) {
        rows->overflow = 1;
        return;
    }
    memcpy(rows->text + rows->used, part, length + 1);
    rows->used += length;
}

/* Строки выборки `sql` совпадают с `expected`; каждая строка уходит в `trace`. */
static int rows_are(sqlite3 *db, const char *sql, const char *expected)
{
    trace("query %s", sql);
    sqlite3_stmt *statement = NULL;
    if (sqlite3_prepare_v2(db, sql, -1, &statement, NULL) != SQLITE_OK) {
        trace("  -> %s", sqlite3_errmsg(db));
        return 0;
    }

    struct rows rows = {0};
    int         result;
    while ((result = sqlite3_step(statement)) == SQLITE_ROW) {
        size_t start = rows.used;
        for (int column = 0; column < sqlite3_column_count(statement); column++) {
            const unsigned char *text = sqlite3_column_text(statement, column);
            append(&rows, column == 0 ? "" : " | ");
            append(&rows, text != NULL ? (const char *)text : "NULL");
        }
        trace("  %s", rows.text + start);
        append(&rows, "\n");
    }

    if (result != SQLITE_DONE) {
        trace("  -> %s", sqlite3_errmsg(db));
    }
    sqlite3_finalize(statement);
    return result == SQLITE_DONE && !rows.overflow && strcmp(rows.text, expected) == 0;
}

static int journal_absent(void)
{
    struct stat st;
    int         absent = stat(JOURNAL, &st) != 0 && errno == ENOENT;
    trace("journal %s", absent ? "absent" : "present");
    return absent;
}

static void check_threadsafe(void)
{
    int mode = sqlite3_threadsafe();
    trace("sqlite3_threadsafe -> %d", mode);
    selftest_check(mode == 1, "sqlite3_threadsafe returns 1");
}

static void check_mutexes(void)
{
    sqlite3_mutex *mutex = sqlite3_mutex_alloc(SQLITE_MUTEX_FAST);
    selftest_check(mutex != NULL, "sqlite allocates a mutex");
    sqlite3_mutex_enter(mutex);

    int result = sqlite3_mutex_try(mutex);
    trace("sqlite3_mutex_try on a held mutex -> %s", sqlite3_errstr(result));
    selftest_check(result == SQLITE_BUSY, "a held sqlite mutex refuses a second owner");

    sqlite3_mutex_leave(mutex);
    sqlite3_mutex_free(mutex);
}

static void check_relative_path(void)
{
    sqlite3 *db = NULL;
    int      result = sqlite3_open_v2("test.db", &db, READ_WRITE, NULL);
    trace("open test.db -> %s", sqlite3_errstr(result));
    selftest_check(result == SQLITE_CANTOPEN, "a relative path fails with SQLITE_CANTOPEN");
    sqlite3_close(db);
}

static void check_reopen(void)
{
    unlink(DATABASE);
    sqlite3 *db = open_database(READ_WRITE);
    selftest_check(db != NULL, "the database opens on /tmp");
    selftest_check(run(db, "CREATE TABLE books(title TEXT, author TEXT, year INTEGER);"
                           "INSERT INTO books VALUES"
                           " ('A Wizard of Earthsea', 'Ursula K. Le Guin', 1968),"
                           " ('The Dispossessed', 'Ursula K. Le Guin', 1974),"
                           " ('Solaris', 'Stanisław Lem', 1961);") == SQLITE_OK,
                   "the table fills");
    selftest_check(sqlite3_close(db) == SQLITE_OK, "the database closes");

    db = open_database(READ_WRITE);
    selftest_check(rows_are(db, BOOKS_BY_YEAR, SHELF), "the rows survive reopening");
    sqlite3_close(db);
}

static void check_rollback(void)
{
    sqlite3 *db = open_database(READ_WRITE);
    selftest_check(run(db, "BEGIN;"
                           "INSERT INTO books VALUES ('The Cyberiad', 'Stanisław Lem', 1965);"
                           "DELETE FROM books WHERE year = 1974;"
                           "ROLLBACK;") == SQLITE_OK,
                   "the transaction rolls back");
    selftest_check(rows_are(db, BOOKS_BY_YEAR, SHELF), "ROLLBACK restores the table");
    selftest_check(journal_absent(), "ROLLBACK leaves no journal");
    sqlite3_close(db);
}

static void check_commit(void)
{
    sqlite3 *db = open_database(READ_WRITE);
    selftest_check(run(db, "BEGIN;"
                           "INSERT INTO books VALUES ('The Cyberiad', 'Stanisław Lem', 1965);"
                           "UPDATE books SET title = 'Solaris (1961)' WHERE title = 'Solaris';"
                           "COMMIT;") == SQLITE_OK,
                   "the transaction commits");
    selftest_check(journal_absent(), "COMMIT leaves no journal");
    sqlite3_close(db);

    db = open_database(READ_WRITE);
    selftest_check(rows_are(db, BOOKS_BY_YEAR,
                            "Solaris (1961) | Stanisław Lem | 1961\n"
                            "The Cyberiad | Stanisław Lem | 1965\n"
                            "A Wizard of Earthsea | Ursula K. Le Guin | 1968\n"
                            "The Dispossessed | Ursula K. Le Guin | 1974\n"),
                   "committed changes survive reopening");
    sqlite3_close(db);
}

static void check_read_only(void)
{
    sqlite3 *db = open_database(SQLITE_OPEN_READONLY);
    selftest_check(rows_are(db, "SELECT count(*) FROM books", "4\n"), "a read-only database returns rows");
    selftest_check(run(db, "INSERT INTO books VALUES ('Fiasco', 'Stanisław Lem', 1986)") == SQLITE_READONLY,
                   "INSERT into a read-only database fails with SQLITE_READONLY");
    selftest_check(rows_are(db, "SELECT count(*) FROM books", "4\n"), "a refused INSERT keeps the rows");
    sqlite3_close(db);
}

static void *insert_rows(void *argument)
{
    sqlite3      *db = argument;
    sqlite3_stmt *statement = NULL;
    if (sqlite3_prepare_v2(db, "INSERT INTO inserts VALUES (?)", -1, &statement, NULL) != SQLITE_OK) {
        trace("thread prepare INSERT INTO inserts -> %s", sqlite3_errmsg(db));
        return NULL;
    }

    int done = 0;
    for (int i = 0; i < THREAD_INSERTS; i++) {
        sqlite3_bind_int(statement, 1, i);
        if (sqlite3_step(statement) == SQLITE_DONE) {
            atomic_fetch_sub(&unfinished_inserts, 1);
            done++;
        }
        sqlite3_reset(statement);
    }
    sqlite3_finalize(statement);

    trace("thread INSERT INTO inserts -> %d of %d done", done, THREAD_INSERTS);
    return NULL;
}

static void check_threads(void)
{
    sqlite3 *db = open_database(READ_WRITE);
    selftest_check(run(db, "CREATE TABLE inserts(n INTEGER)") == SQLITE_OK, "the table for threads is created");

    atomic_store(&unfinished_inserts, INSERTING_THREADS * THREAD_INSERTS);
    pthread_t threads[INSERTING_THREADS];
    int       started = 0;
    for (int i = 0; i < INSERTING_THREADS; i++) {
        if (pthread_create(&threads[started], NULL, insert_rows, db) == 0) {
            started++;
        }
    }
    for (int i = 0; i < started; i++) {
        pthread_join(threads[i], NULL);
    }

    selftest_check(started == INSERTING_THREADS, "the inserting threads start");
    selftest_check(atomic_load(&unfinished_inserts) == 0, "every insert from the threads returns SQLITE_DONE");
    selftest_check(rows_are(db, "SELECT count(*) FROM inserts", "400\n"), "the threads insert every row");
    sqlite3_close(db);
}

int selftest_main(int argc, char **argv)
{
    (void)argc;
    (void)argv;
    check_threadsafe();
    check_mutexes();
    check_relative_path();
    check_reopen();
    check_rollback();
    check_commit();
    check_read_only();
    check_threads();
    return failures == 0 ? 0 : 1;
}
