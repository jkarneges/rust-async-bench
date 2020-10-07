# Rust Async Benchmark

This project attempts to compare the performance of a manually written poll loop vs async/await. It uses "fake" I/O objects that work in memory. The async executor uses no allocs, mutexes, or thread local storage, and tries to be efficient about when to make I/O calls.

## Run and print I/O call usage

```
$ cargo run
    Finished dev [unoptimized + debuginfo] target(s) in 0.02s
     Running `target/debug/rust-async-bench`
sync:  StatsMetrics { register: 33, unregister: 33, poll: 34, accept: 64, read: 64, write: 64 }
async: StatsMetrics { register: 33, unregister: 33, poll: 34, accept: 64, read: 64, write: 64 }
```

## Run benchmark

```
cargo bench
```

Some results running on Linux:

```
run_sync                time:   [4.7783us 4.7919us 4.8089us]
run_async               time:   [16.847us 16.928us 17.025us]
run_sync_with_syscalls  time:   [138.07us 141.20us 147.21us]
run_async_with_syscalls time:   [152.09us 153.50us 155.30us]
```

## Analysis

Is async Rust "zero cost"?

The non-async benchmarks win, and the async engine in this project is borderline contrived and probably cannot be meaningfully optimized further. Thus, it is safe to say there is a cost to async Rust. However, it is important to put this in perspective:

* The cost is only some in-app state and function calls. Async Rust does not require heap allocations, threading primitives, or other conventionally costly operations. The number of system calls can be kept the same as a poll loop.

* The cost of doing anything meaningful in an application will likely dwarf the cost of async execution. For example, merely adding bogus system calls closes the gap between the benchmarks considerably, with the non-async implementation being only 9% faster.

* The benchmarks test 32 requests. The difference between the async and non-async syscall benchmarks is 12.3us. Divided by 32, that's an overhead of around 400ns per request. In a server app, that's practically free.
