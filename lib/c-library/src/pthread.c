/* Значения pthread.h для Rust-части c-library и функции pthread со сроком. */

#include <pthread.h>
#include <stdint.h>

const int    c_library_pthread_create_joinable = PTHREAD_CREATE_JOINABLE;
const int    c_library_pthread_create_detached = PTHREAD_CREATE_DETACHED;
const size_t c_library_pthread_stack_min = PTHREAD_STACK_MIN;

const int c_library_pthread_mutex_normal = PTHREAD_MUTEX_NORMAL;
const int c_library_pthread_mutex_recursive = PTHREAD_MUTEX_RECURSIVE;
const int c_library_pthread_mutex_errorcheck = PTHREAD_MUTEX_ERRORCHECK;
const int c_library_pthread_mutex_default = PTHREAD_MUTEX_DEFAULT;

int c_library_mutex_timedlock(pthread_mutex_t *mutex, int64_t seconds, int64_t nanoseconds);
/* Пустые часы clock означают часы переменной. */
int c_library_cond_timedwait(pthread_cond_t *cond, pthread_mutex_t *mutex, const clockid_t *clock,
                             int64_t seconds, int64_t nanoseconds);

int pthread_mutex_timedlock(pthread_mutex_t *__restrict mutex,
                            const struct timespec *__restrict abstime)
{
    return c_library_mutex_timedlock(mutex, abstime->tv_sec, abstime->tv_nsec);
}

int pthread_cond_timedwait(pthread_cond_t *__restrict cond, pthread_mutex_t *__restrict mutex,
                           const struct timespec *__restrict abstime)
{
    return c_library_cond_timedwait(cond, mutex, NULL, abstime->tv_sec, abstime->tv_nsec);
}

int pthread_cond_clockwait(pthread_cond_t *__restrict cond, pthread_mutex_t *__restrict mutex,
                           clockid_t clock, const struct timespec *__restrict abstime)
{
    return c_library_cond_timedwait(cond, mutex, &clock, abstime->tv_sec, abstime->tv_nsec);
}
