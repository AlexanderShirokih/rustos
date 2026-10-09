/*
 * Системный слой времени библиотеки C поверх часов и сна c-library.
 *
 * Файл раскладывает показания Rust-части в struct timespec и struct timeval заголовков.
 * Значения CLOCK_* Rust-часть получает отсюда константами.
 */

/* Заголовки picolibc объявляют CLOCK_MONOTONIC под _GNU_SOURCE. */
#define _GNU_SOURCE

#include <stddef.h>
#include <stdint.h>
#include <sys/time.h>
#include <time.h>

const clockid_t c_library_clock_realtime = CLOCK_REALTIME;
const clockid_t c_library_clock_monotonic = CLOCK_MONOTONIC;

/* Показание или разрешение часов от Rust-части. */
struct c_library_time {
    int64_t seconds;
    int64_t nanoseconds;
};

int c_library_clock_read(clockid_t clock, struct c_library_time *time);
int c_library_clock_resolution(clockid_t clock, struct c_library_time *time);
int c_library_sleep(int64_t seconds, int64_t nanoseconds);

int clock_gettime(clockid_t clock, struct timespec *ts)
{
    struct c_library_time time;
    if (c_library_clock_read(clock, &time) != 0) {
        return -1;
    }
    ts->tv_sec = time.seconds;
    ts->tv_nsec = time.nanoseconds;
    return 0;
}

int clock_getres(clockid_t clock, struct timespec *res)
{
    struct c_library_time time;
    if (c_library_clock_resolution(clock, &time) != 0) {
        return -1;
    }
    if (res != NULL) {
        res->tv_sec = time.seconds;
        res->tv_nsec = time.nanoseconds;
    }
    return 0;
}

int nanosleep(const struct timespec *request, struct timespec *remaining)
{
    (void)remaining;
    return c_library_sleep(request->tv_sec, request->tv_nsec);
}

int gettimeofday(struct timeval *__restrict tv, void *__restrict tz)
{
    struct c_library_time time;
    (void)tz;
    if (c_library_clock_read(CLOCK_REALTIME, &time) != 0) {
        return -1;
    }
    tv->tv_sec = time.seconds;
    tv->tv_usec = (suseconds_t)(time.nanoseconds / 1000);
    return 0;
}
