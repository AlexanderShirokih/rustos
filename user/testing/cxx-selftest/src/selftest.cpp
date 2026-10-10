#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <iostream>
#include <mutex>
#include <pthread.h>
#include <stdexcept>
#include <thread>
#include <vector>

namespace {

// Потоки проверок и число исключений, которое выбрасывает и ловит каждый поток.
constexpr int kThreads = 4;
constexpr int kThrowsPerThread = 100;

// Глубина вызовов, с которой выбрасывается исключение.
constexpr int kThrowDepth = 5;

// Thread-local переменная: начальное значение и значение, которое оставляет поток.
constexpr int kCounterInitial = 5;
constexpr int kCounterLeft = 42;

// Значение, которое конструктор локальной статической переменной кладёт в объект.
constexpr int kReadyValue = 7;

constexpr auto kShortWait = std::chrono::milliseconds(50);

constexpr const char *kThrownMessage = "thrown deep";

class StandError : public std::runtime_error {
public:
    explicit StandError(const char *message) : std::runtime_error(message) {}
};

// Счётчик кадров, которые покинуло исключение.
std::atomic<int> unwound_frames{0};

struct FrameGuard {
    ~FrameGuard() { unwound_frames.fetch_add(1); }
};

thread_local int thread_counter = kCounterInitial;

std::atomic<int> destructor_saw_counter{0};
std::atomic<bool> destructor_ran_on_owner{false};
std::atomic<bool> destructor_saw_key{false};

// Ключ с деструктором и значение, которое под ним оставляет поток.
pthread_key_t departure_key;
int departure_marker = 0;

void forget_departure(void *) {}

// Объект потока, деструктор которого читает другую thread_local переменную.
struct Departing {
    std::thread::id owner = std::this_thread::get_id();

    ~Departing()
    {
        destructor_saw_counter.store(thread_counter);
        destructor_ran_on_owner.store(owner == std::this_thread::get_id());
        destructor_saw_key.store(pthread_getspecific(departure_key) == &departure_marker);
    }
};

thread_local Departing departing;

std::atomic<int> constructions{0};

// Объект локальной статической переменной: конструктор считает вызовы и задерживает
// инициализацию, пока к ней обращаются другие потоки.
struct Counted {
    int value;

    Counted() : value(kReadyValue)
    {
        constructions.fetch_add(1);
        std::this_thread::sleep_for(kShortWait);
    }
};

void check(bool passed, const char *what)
{
    if (!passed) {
        std::fprintf(stderr, "check failed: %s\n", what);
        std::exit(1);
    }
}

[[gnu::noinline]] void throw_from(int depth)
{
    FrameGuard guard;
    if (depth == 0) {
        throw StandError(kThrownMessage);
    }
    throw_from(depth - 1);
}

template <typename Body> std::vector<bool> on_threads(Body body)
{
    std::vector<char> results(kThreads, 0);
    std::vector<std::thread> threads;
    for (int index = 0; index < kThreads; index++) {
        threads.emplace_back([&results, &body, index] { results[index] = body(index); });
    }
    for (std::thread &thread : threads) {
        thread.join();
    }
    return std::vector<bool>(results.begin(), results.end());
}

bool all(const std::vector<bool> &results)
{
    for (bool result : results) {
        if (!result) {
            return false;
        }
    }
    return true;
}

void check_exceptions()
{
    unwound_frames.store(0);
    bool caught = false;
    try {
        throw_from(kThrowDepth);
    } catch (const std::exception &error) {
        caught = std::strcmp(error.what(), kThrownMessage) == 0 &&
                 unwound_frames.load() == kThrowDepth + 1;
    }
    check(caught, "an exception reaches a base class handler through frames");

    bool threads_caught = all(on_threads([](int) {
        int caught = 0;
        for (int round = 0; round < kThrowsPerThread; round++) {
            try {
                throw_from(kThrowDepth);
            } catch (const StandError &) {
                caught++;
            }
        }
        return caught == kThrowsPerThread;
    }));
    check(threads_caught, "threads catch their exceptions concurrently");
}

void check_thread_locals()
{
    bool own = all(on_threads([](int index) {
        bool fresh = thread_counter == kCounterInitial;
        for (int step = 0; step <= index; step++) {
            thread_counter++;
        }
        return fresh && thread_counter == kCounterInitial + index + 1;
    }));
    check(own && thread_counter == kCounterInitial, "each thread has its own thread_local");

    std::thread([] {
        pthread_setspecific(departure_key, &departure_marker);
        static_cast<void>(departing.owner);
        thread_counter = kCounterLeft;
    }).join();
    check(destructor_saw_counter.load() == kCounterLeft && destructor_ran_on_owner.load(),
          "a thread_local destructor runs on its thread and reads its thread_locals");
    check(destructor_saw_key.load(), "a thread_local destructor runs before the key destructors");
}

void check_local_static()
{
    bool ready = all(on_threads([](int) {
        static Counted counted;
        return counted.value == kReadyValue;
    }));
    check(ready && constructions.load() == 1, "a local static initializes once");
}

void check_waits()
{
    std::mutex mutex;
    std::condition_variable changed;
    std::unique_lock<std::mutex> lock(mutex);
    auto start = std::chrono::steady_clock::now();
    std::cv_status status = changed.wait_for(lock, kShortWait);
    auto elapsed = std::chrono::steady_clock::now() - start;
    check(status == std::cv_status::timeout && elapsed >= kShortWait,
          "wait_for times out without the realtime clock");

    bool waiting = false;
    bool notified = false;
    bool woke_notified = false;
    std::thread waiter([&] {
        std::unique_lock<std::mutex> waiter_lock(mutex);
        waiting = true;
        changed.notify_one();
        changed.wait(waiter_lock, [&] { return notified; });
        woke_notified = notified;
    });
    changed.wait(lock, [&] { return waiting; });
    notified = true;
    lock.unlock();
    changed.notify_one();
    waiter.join();
    check(woke_notified, "a waiting thread wakes with the changed condition");
}

} // namespace

int main()
{
    std::cout << "selftest started" << std::endl;
    check(pthread_key_create(&departure_key, forget_departure) == 0, "a key is created");
    check_exceptions();
    check_thread_locals();
    check_local_static();
    check_waits();
    std::cout << "checks passed" << std::endl;
    throw std::runtime_error("selftest leaves main");
}
