# Rust Async Benchmark

This project compares the performance of a manually written event loop vs async/await, in Rust.

To be clear, this is not a comparison of thread pools vs coroutines. It's also not a comparison of async runtimes. It's a comparison of single-threaded manually written event loop code vs single-threaded async/await code, to determine the minimum overhead of adopting async in a Rust application.

## Goals

* Determine to what extent async Rust may require conventionally costly operations, such as heap allocs or syscalls.
* Compare the CPU usage of a manually written event loop to a minimal async executor.
* Measure the CPU costs of some fundamental async implementation choices, such as boxed futures and atomic wakers.
* Answer the question: is async Rust a "zero cost abstraction"?

## How it works

In order to simulate a somewhat realistic application style, the benchmarks implement a fake network server that responds to requests. The fake server accepts "connections", which are bidirectional streams of bytes. For each connection, it reads a line of text as a request, then it writes a line of text as a response. Multiple variations of this fake server are implemented. One variation uses a manually written event loop and the rest use async.

Each async benchmark uses one of the following executors:

* `ArgExecutor`: The most minimal executor. It supports exactly one `Future` type, passing an argument to the future when spawning. Multiple kinds of task logic can be supported by having the one future type call out to different async functions depending on the argument. Futures are not boxed (they are stored by value in a `Vec`), and waker data is not separately allocated.

* `BoxExecutor`: Supports arbitrary `Future` types by boxing them. Otherwise, it works similarly to `ArgExecutor` in that waker data is not separately allocated.

* `BoxRcExecutor`: A more conventional executor, that supports arbitrary `Future` types by boxing them, and allocates ref-counted wakers. It supports both Rc-based and Arc-based wakers.

The benchmarks are:

* `manual`: A manually written event loop, to use as a basis for comparison.
* `nonbox`: Uses `ArgExecutor`, the most minimal executor. It uses no heap allocs at runtime, but all futures take up the same amount of space.
* `callerbox`: Like `nonbox`, but the caller boxes the futures and uses the box as the one future type to execute. This works because the standard library implements `Future` for `Pin<Box<dyn Future>>`. It boxes the same future type used by the `nonbox` benchmark, so all futures still take up the same amount of space.
* `large+nonbox`: Like `nonbox`, but a larger future is used.
* `box`: Uses `BoxExecutor`. This means heap allocs are used at runtime, but different futures can take up different amounts of space.
* `box+callerbox`: Like `box`, but the caller boxes the futures instead of the executor doing the boxing.
* `large+box`: Like `box`, but a larger future is used.
* `box+rc`: Uses `BoxRcExecutor` with an unsafe Rc-based waker. Using such a waker from another thread would cause undefined behavior.
* `box+chkrc`: Uses `BoxRcExecutor` with a "safe" Rc-based waker. The waker has thread-affinity and panics if it is used from another thread.
* `box+arc`: Uses `BoxRcExecutor` with an Arc-based waker (`std::task::Wake`).

Additionally, there are variations of these benchmarks that make I/O syscalls, suffixed with `+syscalls`.

Each benchmark performs 256 request/response transactions.

### I/O counts

To count I/O calls for the manual event loop and minimal async executor, run `cargo run`:

```
$ cargo run
    Finished dev [unoptimized + debuginfo] target(s) in 0.03s
     Running `target/debug/rust-async-bench`
manual: register=257 unregister=257 poll=258 accept=512 read=512 write=512
async: register=257 unregister=257 poll=258 accept=512 read=512 write=512
```

(Note: since this only counts operations and is not a speed test, the build mode doesn't matter.)

Running `cargo test` will verify these expected counts for all implementation variations.

## Benchmarks

To measure the speed of the manual event loop vs. the various async implementations/configurations, run `cargo bench`.

Some results running on Linux:

```
manual                  time:   [24.536 us 24.548 us 24.562 us]
nonbox                  time:   [90.702 us 90.724 us 90.750 us]
callerbox               time:   [89.458 us 89.552 us 89.686 us]
large+nonbox            time:   [186.62 us 186.65 us 186.68 us]
box                     time:   [98.172 us 98.187 us 98.203 us]
box+callerbox           time:   [97.099 us 97.143 us 97.201 us]
large+box               time:   [212.26 us 212.33 us 212.40 us]
box+rc                  time:   [95.511 us 95.531 us 95.553 us]
box+chkrc               time:   [111.32 us 111.33 us 111.35 us]
box+arc                 time:   [102.16 us 102.18 us 102.19 us]
manual+syscalls         time:   [991.44 us 991.72 us 992.00 us]
nonbox+syscalls         time:   [1.0841 ms 1.0846 ms 1.0851 ms]
box+syscalls            time:   [1.1009 ms 1.1013 ms 1.1019 ms]
box+rc+syscalls         time:   [1.0941 ms 1.0944 ms 1.0948 ms]
box+chkrc+syscalls      time:   [1.1037 ms 1.1047 ms 1.1058 ms]
box+arc+syscalls        time:   [1.1041 ms 1.1045 ms 1.1049 ms]
```

## Analysis

### Summary

* Async may require 9.3% more CPU than hand written code, but likely much less in real applications.
* Boxing futures has relatively insignificant cost and is worth it for flexibility and ergonomics.
* Arc-based wakers are the best we've got.
* Async possibly qualifies as a zero cost abstraction if you've already bought into the `Future` trait. In practice it may have a (very low) cost compared to code that wouldn't have used `Future` or similar.

### Costs

All of the async benchmarks are slower than the `manual` benchmark, so async Rust has a measurable cost.

That said, async Rust does not require the use of any conventionally costly operations, such as heap allocs, atomics, mutexes, or thread local storage, nor does it require introducing extra syscalls, buffer copies, or algorithms with worse big O complexity, compared to what would be required by a manually written poll loop. Not even a hashmap is required. At minimum, the required costs involve shuffling around bools, ints, and references, and using collections with fast O(1) operations such as arrays and linked lists. This is proven by `ArgExecutor`.

Boxing tasks has an additional cost (see `nonbox` vs. `box` and `large+nonbox` vs. `large+box`). This is not really surprising.

Embedding wakers in existing data structures instead of separately allocating them doesn't make a meaningful difference (see `box` vs. `box+rc`).

Arc-based wakers are slower than unsafe Rc-based wakers (see `box+rc` vs. `box+arc`). However, they are faster than checked Rc-based wakers (see `box+chkrc` vs. `box+arc`).

### Relative costs

In a real application, task accounting should normally be a very small part of overall compute time. The benchmarks with syscalls help highlight this. These benchmarks make a non-blocking call to `libc::read` whenever there is an I/O operation. The read is against a pipe that never has data in it, and so the call always returns an error. Adding in these syscalls significantly reduces the time differences between the benchmarks. For example, `manual` is 73% faster than `nonbox`, but `manual+syscalls` is only 8.6% faster than `nonbox+syscalls` (or reversed, `nonbox+syscalls` is 9.3% slower).

These no-op syscalls only scratch the surface in terms of what a real application might do. In a real application, I/O syscalls will likely do meaningful things (i.e. read a non-zero-sized network packet) and thus cost more. Real applications will also perform real application logic. The benchmarks test 256 requests. If an application were to spend even 10 microseconds per request doing meaningful work, the manual event loop would only be 2.6% faster.

Another way of looking at: the difference between `manual` and `nonbox` is 66.1us. Divided by 256, that's an overhead of around 258 nanoseconds for async execution per request. In a real app, that's practically free.

### Boxing futures

Boxing futures has a small cost (`nonbox+syscalls` is 1.6% faster than `box+syscalls`). However, not boxing has drawbacks too: all futures take up the same amount of space, and spawning needs to be done indirectly to avoid circular references. Also, unless the space for all futures is allocated up front, allocations will need to be made at runtime anyway.

Given the small relative cost of boxing, the cost is well worth it for the flexibility and ergonomics it brings. A high performance network server running on a typical OS should box its futures.

The only time to avoid boxing might be in hard real-time applications, for example games with frame-rendering deadlines, or embedded systems without support for allocations.

### Rc vs Arc wakers

In theory, Rc-based wakers have legitimate value, in that they are faster than Arc-based wakers (`box+rc+syscalls` is 1% faster than `box+arc+syscalls`) and can simply be dropped in where applicable (single-threaded executors). Unfortunately, it is currently not possible to use Rc-based wakers safely without sacrificing their performance gains.

Using unsafe wakers is probably not worth it for their small relative gains. It seems like it would be a hard thing to audit. Waker construction could be marked `unsafe`, but waker proliferation and use would not be.

Arc-based wakers are faster than safe Rc-based wakers, at least when there is no contention on the wakers between multiple threads. And if you're using single-threaded executors, then there should be no contention. This means Arc-based wakers are the safest, fastest choice for either single-threaded or multi-threaded executors.

### Zero cost?

Is async Rust a "zero cost abstraction"?

Let's start by quoting Bjarne Stroustrup's definition: "What you don't use, you don't pay for. And further: What you do use, you couldn't hand code any better."

If you don't use async at all, then you don't pay anything for it. That much is true. However, the `manual` benchmark beats `nonbox`, suggesting that a non-blocking event loop can be hand-coded better than by using async.

Async Rust has different goals than that manually written code though, such as encapsulation (hiding everything behind a poll() method) and reactor/executor decoupling (wakers). If you've already bought into using the `Future` trait, then async generators may very well be zero cost. But if your hand written code wouldn't normally have used `Future` or something like it, then adopting the whole of async will have a cost. Speaking for myself, I've never implemented `Future`-like encapsulation or decoupling in my own event loop code. Probably this is because the code was always application-specific and not part of a composable framework.

The relative cost of async is low though, and it comes with the huge benefit of being able to write code that is much easier to reason about and to extend. Again speaking for myself, I'll admit I've made compromises in the control flows of my own event loop code, in order to increase maintainability and comprehension at the cost of correctness. Async makes it practical to implement complex control flow correctly, that is even easier to maintain and comprehend than without.

## Implementation details

In general, the code in this project is written in a mostly-safe, mostly-idiomatic manner. Everything is designed to be performant, by selecting good algorithms and avoiding conventionally costly operations. It may be possible to make things faster by merging reactor and executor logic, or by not using wakers, or by writing more unsafe code, but I felt I drew a reasonable line.

This project uses "fake" I/O objects that work in memory. The I/O primitives are `FakeListener`, `FakeStream`, and `Poll`, analogous to `TcpListener`, `TcpStream`, and Mio's `Poll`. There is no client side, and thus no client-side overhead when benchmarking.

There are two kinds of tasks to perform: accept connections and process connections. The manual benchmark is implemented as a poll loop with all tasks intermingled. The async benchmarks are implemented using individual future instances for each task, that are then executed concurrently.

All variations can be run with or without syscalls. When syscalls are enabled, `libc::read` is called on an empty pipe every time there would have been an I/O operation.

It is relatively straightforward to write a single-threaded poll loop server that doesn't use heap allocations or synchronization primitives. Doing the same with async/await, and doing it without making extra syscalls, is a bit trickier. The following techniques are used:

* I/O objects register/unregister with the poller when they are initialized/dropped as opposed to when I/O futures are used. They also keep track of their readiness state at all times. This helps reduce the overhead of the I/O futures. For example, if a stream is known to be not readable and `read()` is called on it, the returned future will immediately return `Pending` when polled, without performing a syscall.

* `ArgExecutor` is generic over a single future type, `F`, and it stores the futures as non-boxed values. In order to support two kinds of tasks with only one future type, the accept handler and connection handler are implemented within the same async function, and the desired task is selected via argument. This way we can avoid heap allocations when spawning, at the cost of all the futures taking up the same amount of memory.

* In `ArgExecutor`, the waker points at a struct that is known not to move for the lifetime of a future, and this struct contains references to the associated executor and task. This enables the waker to find the executor and the task it is responsible for, without having to do any heap allocations on its own or use thread local storage to find the executor. For this to be safe, a waker (or more specifically the underlying shared data of a waker, as a waker can be cloned) must not outlive the future it was created for. This is a pretty reasonable condition to adhere to, and the executor asserts it at runtime whenever a future completes.

* Lifetime annotations everywhere! There is no `Rc` used in the `fakeio` module or `ArgExecutor`, and all shared objects are passed along as references. The reactor must live as long as the executor and the I/O objects, the executor must live as long as the top-level futures, the top-level futures must live as long as the I/O objects, and the I/O objects must live as long as the I/O futures. Somehow it all works. The Rust compiler is amazing.
