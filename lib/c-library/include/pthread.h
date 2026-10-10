/*
 * Потоки POSIX библиотеки C.
 *
 * Функции даёт системный слой c-library. Раскладку типов повторяет его Rust-часть.
 */

#ifndef _PTHREAD_H
#define _PTHREAD_H

#include <stddef.h>
#include <sys/cdefs.h>
#include <time.h>

_BEGIN_STD_C

#define PTHREAD_CREATE_JOINABLE 0
#define PTHREAD_CREATE_DETACHED 1

#define PTHREAD_STACK_MIN 16384

#define PTHREAD_KEYS_MAX              128
#define PTHREAD_DESTRUCTOR_ITERATIONS 4

#define PTHREAD_MUTEX_NORMAL     0
#define PTHREAD_MUTEX_RECURSIVE  1
#define PTHREAD_MUTEX_ERRORCHECK 2
#define PTHREAD_MUTEX_DEFAULT    PTHREAD_MUTEX_NORMAL

#define PTHREAD_MUTEX_INITIALIZER  {{0}}
#define PTHREAD_COND_INITIALIZER   {{0}}
#define PTHREAD_RWLOCK_INITIALIZER {{0}}
#define PTHREAD_ONCE_INIT          0

typedef struct __pthread *pthread_t;

typedef unsigned int pthread_key_t;

typedef struct {
    size_t __stack_bytes;
    int    __detach_state;
} pthread_attr_t;

typedef union {
    unsigned char __bytes[32];
    long long     __align;
} pthread_mutex_t;

typedef struct {
    int __kind;
} pthread_mutexattr_t;

typedef union {
    unsigned char __bytes[12];
    unsigned int  __align;
} pthread_cond_t;

typedef struct {
    int __clock;
} pthread_condattr_t;

typedef union {
    unsigned char __bytes[20];
    unsigned int  __align;
} pthread_rwlock_t;

typedef struct {
    int __reserved;
} pthread_rwlockattr_t;

typedef int pthread_once_t;

#ifndef __cplusplus
_Static_assert(sizeof(pthread_attr_t) == 16 && _Alignof(pthread_attr_t) == 8,
               "pthread_attr_t layout is shared with c-library");
_Static_assert(sizeof(pthread_mutex_t) == 32 && _Alignof(pthread_mutex_t) == 8,
               "pthread_mutex_t layout is shared with c-library");
_Static_assert(sizeof(pthread_cond_t) == 12 && _Alignof(pthread_cond_t) == 4,
               "pthread_cond_t layout is shared with c-library");
_Static_assert(sizeof(pthread_rwlock_t) == 20 && _Alignof(pthread_rwlock_t) == 4,
               "pthread_rwlock_t layout is shared with c-library");
#endif

int pthread_create(pthread_t *__restrict thread, const pthread_attr_t *__restrict attr,
                   void *(*start)(void *), void *__restrict arg);
int pthread_join(pthread_t thread, void **value);
int pthread_detach(pthread_t thread);
/* В главном потоке pthread_exit завершает процесс через abort. */
__noreturn void pthread_exit(void *value);
pthread_t       pthread_self(void);
int             pthread_equal(pthread_t first, pthread_t second);

int pthread_attr_init(pthread_attr_t *attr);
int pthread_attr_destroy(pthread_attr_t *attr);
int pthread_attr_setstacksize(pthread_attr_t *attr, size_t stack_bytes);
int pthread_attr_getstacksize(const pthread_attr_t *__restrict attr,
                              size_t *__restrict stack_bytes);
int pthread_attr_setdetachstate(pthread_attr_t *attr, int detach_state);
int pthread_attr_getdetachstate(const pthread_attr_t *attr, int *detach_state);

/* Снятый ключ не выдаётся повторно: за время жизни процесса доступно PTHREAD_KEYS_MAX ключей. */
int   pthread_key_create(pthread_key_t *key, void (*destructor)(void *));
int   pthread_key_delete(pthread_key_t key);
void *pthread_getspecific(pthread_key_t key);
int   pthread_setspecific(pthread_key_t key, const void *value);

int pthread_mutex_init(pthread_mutex_t *__restrict mutex,
                       const pthread_mutexattr_t *__restrict attr);
int pthread_mutex_destroy(pthread_mutex_t *mutex);
int pthread_mutex_lock(pthread_mutex_t *mutex);
int pthread_mutex_trylock(pthread_mutex_t *mutex);
int pthread_mutex_timedlock(pthread_mutex_t *__restrict mutex,
                            const struct timespec *__restrict abstime);
int pthread_mutex_unlock(pthread_mutex_t *mutex);

int pthread_mutexattr_init(pthread_mutexattr_t *attr);
int pthread_mutexattr_destroy(pthread_mutexattr_t *attr);
int pthread_mutexattr_settype(pthread_mutexattr_t *attr, int kind);
int pthread_mutexattr_gettype(const pthread_mutexattr_t *__restrict attr, int *__restrict kind);

int pthread_cond_init(pthread_cond_t *__restrict cond, const pthread_condattr_t *__restrict attr);
int pthread_cond_destroy(pthread_cond_t *cond);
int pthread_cond_wait(pthread_cond_t *__restrict cond, pthread_mutex_t *__restrict mutex);
int pthread_cond_timedwait(pthread_cond_t *__restrict cond, pthread_mutex_t *__restrict mutex,
                           const struct timespec *__restrict abstime);
/* Срок отсчитывается по часам clock независимо от часов переменной. */
int pthread_cond_clockwait(pthread_cond_t *__restrict cond, pthread_mutex_t *__restrict mutex,
                           clockid_t clock, const struct timespec *__restrict abstime);
int pthread_cond_signal(pthread_cond_t *cond);
int pthread_cond_broadcast(pthread_cond_t *cond);

int pthread_condattr_init(pthread_condattr_t *attr);
int pthread_condattr_destroy(pthread_condattr_t *attr);
int pthread_condattr_setclock(pthread_condattr_t *attr, clockid_t clock);
int pthread_condattr_getclock(const pthread_condattr_t *__restrict attr,
                              clockid_t *__restrict clock);

/* Блокировка ведёт себя одинаково при любых атрибутах. */
int pthread_rwlock_init(pthread_rwlock_t *__restrict rwlock,
                        const pthread_rwlockattr_t *__restrict attr);
int pthread_rwlock_destroy(pthread_rwlock_t *rwlock);
int pthread_rwlock_rdlock(pthread_rwlock_t *rwlock);
int pthread_rwlock_wrlock(pthread_rwlock_t *rwlock);
int pthread_rwlock_unlock(pthread_rwlock_t *rwlock);

int pthread_once(pthread_once_t *once, void (*init)(void));

/* sched.h picolibc объявляет sched_yield только для RTEMS. */
int sched_yield(void);

_END_STD_C

#endif /* _PTHREAD_H */
