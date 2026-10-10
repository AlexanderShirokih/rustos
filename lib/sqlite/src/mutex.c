/*
 * Мьютексы sqlite поверх `pthread_mutex` библиотеки C.
 *
 * Динамический мьютекс `SQLITE_MUTEX_FAST` - обычный `pthread_mutex`, `SQLITE_MUTEX_RECURSIVE` -
 * рекурсивный. Статические мьютексы sqlite - рекурсивные и живут до завершения процесса.
 */

#include <pthread.h>
#include <stdlib.h>

#include "mutex.h"

#define STATIC_MUTEXES (SQLITE_MUTEX_STATIC_VFS3 - SQLITE_MUTEX_STATIC_MAIN + 1)

struct sqlite3_mutex {
    pthread_mutex_t lock;
    int             id;
};

static struct sqlite3_mutex statics[STATIC_MUTEXES];

static int init_lock(pthread_mutex_t *lock, int type)
{
    pthread_mutexattr_t attr;
    if (pthread_mutexattr_init(&attr) != 0) {
        return SQLITE_ERROR;
    }

    int result = pthread_mutexattr_settype(&attr, type);
    if (result == 0) {
        result = pthread_mutex_init(lock, &attr);
    }
    pthread_mutexattr_destroy(&attr);
    return result == 0 ? SQLITE_OK : SQLITE_ERROR;
}

static int dynamic(int id)
{
    return id == SQLITE_MUTEX_FAST || id == SQLITE_MUTEX_RECURSIVE;
}

static int mutex_init(void)
{
    for (int i = 0; i < STATIC_MUTEXES; i++) {
        int result = init_lock(&statics[i].lock, PTHREAD_MUTEX_RECURSIVE);
        if (result != SQLITE_OK) {
            return result;
        }
        statics[i].id = SQLITE_MUTEX_STATIC_MAIN + i;
    }
    return SQLITE_OK;
}

static int mutex_end(void)
{
    for (int i = 0; i < STATIC_MUTEXES; i++) {
        pthread_mutex_destroy(&statics[i].lock);
    }
    return SQLITE_OK;
}

static sqlite3_mutex *mutex_alloc(int id)
{
    if (dynamic(id)) {
        sqlite3_mutex *mutex = malloc(sizeof *mutex);
        if (mutex == NULL) {
            return NULL;
        }

        int type = id == SQLITE_MUTEX_RECURSIVE ? PTHREAD_MUTEX_RECURSIVE : PTHREAD_MUTEX_NORMAL;
        if (init_lock(&mutex->lock, type) != SQLITE_OK) {
            free(mutex);
            return NULL;
        }
        mutex->id = id;
        return mutex;
    }

    if (id < SQLITE_MUTEX_STATIC_MAIN || id > SQLITE_MUTEX_STATIC_VFS3) {
        return NULL;
    }
    return &statics[id - SQLITE_MUTEX_STATIC_MAIN];
}

static void mutex_free(sqlite3_mutex *mutex)
{
    if (dynamic(mutex->id)) {
        pthread_mutex_destroy(&mutex->lock);
        free(mutex);
    }
}

static void mutex_enter(sqlite3_mutex *mutex)
{
    pthread_mutex_lock(&mutex->lock);
}

static int mutex_try(sqlite3_mutex *mutex)
{
    return pthread_mutex_trylock(&mutex->lock) == 0 ? SQLITE_OK : SQLITE_BUSY;
}

static void mutex_leave(sqlite3_mutex *mutex)
{
    pthread_mutex_unlock(&mutex->lock);
}

const sqlite3_mutex_methods molecular_mutex_methods = {
    .xMutexInit = mutex_init,
    .xMutexEnd = mutex_end,
    .xMutexAlloc = mutex_alloc,
    .xMutexFree = mutex_free,
    .xMutexEnter = mutex_enter,
    .xMutexTry = mutex_try,
    .xMutexLeave = mutex_leave,
};
