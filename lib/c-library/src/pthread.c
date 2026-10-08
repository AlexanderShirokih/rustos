/* Значения pthread.h для Rust-части c-library. */

#include <pthread.h>

const int    c_library_pthread_create_joinable = PTHREAD_CREATE_JOINABLE;
const int    c_library_pthread_create_detached = PTHREAD_CREATE_DETACHED;
const size_t c_library_pthread_stack_min = PTHREAD_STACK_MIN;

const int c_library_pthread_mutex_normal = PTHREAD_MUTEX_NORMAL;
const int c_library_pthread_mutex_recursive = PTHREAD_MUTEX_RECURSIVE;
const int c_library_pthread_mutex_errorcheck = PTHREAD_MUTEX_ERRORCHECK;
const int c_library_pthread_mutex_default = PTHREAD_MUTEX_DEFAULT;
