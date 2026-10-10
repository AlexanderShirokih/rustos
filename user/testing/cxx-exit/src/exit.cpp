#include <cstdio>
#include <cstring>
#include <pthread.h>
#include <stdexcept>

namespace {

struct Global {
    Global() { std::puts("global constructed"); }
    ~Global() { std::puts("global destroyed"); }
};

struct Traced {
    const char *name;

    explicit Traced(const char *name) : name(name) {}
    ~Traced() { std::printf("%s destroyed\n", name); }
};

Global global;

bool catches(const char *message)
{
    try {
        throw std::runtime_error(message);
    } catch (const std::runtime_error &error) {
        return std::strcmp(error.what(), message) == 0;
    }
}

void touch_first_thread_local()
{
    thread_local Traced first("first thread_local");
    static_cast<void>(first.name);
}

void touch_second_thread_local()
{
    thread_local Traced second("second thread_local");
    static_cast<void>(second.name);
}

void touch_local_static()
{
    static Traced local("local static");
    static_cast<void>(local.name);
}

bool create_all_keys()
{
    for (int index = 0; index < PTHREAD_KEYS_MAX; index++) {
        pthread_key_t key;
        if (pthread_key_create(&key, nullptr) != 0) {
            return false;
        }
    }
    return true;
}

} // namespace

int main()
{
    if (catches("before keys")) {
        touch_first_thread_local();
        std::puts("first thread_local touched");
    }

    touch_local_static();
    std::puts("local static touched");

    if (create_all_keys()) {
        std::puts("all keys created");
    }

    if (catches("after keys")) {
        touch_second_thread_local();
        std::puts("second thread_local touched");
    }

    std::puts("main returns");
    return 0;
}
