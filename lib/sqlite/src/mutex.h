/* Мьютексы sqlite поверх `pthread_mutex` библиотеки C. */

#ifndef SQLITE_MOLECULAR_MUTEX_H
#define SQLITE_MOLECULAR_MUTEX_H

#include "sqlite3.h"

/* Методы для `sqlite3_config(SQLITE_CONFIG_MUTEX)`. */
extern const sqlite3_mutex_methods molecular_mutex_methods;

#endif
