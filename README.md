# Rust Async Benchmark

This project attempts to compare the performance of a manually written poll loop vs async/await. It uses "fake" I/O objects that work in memory. The async executor uses no allocs, mutexes, or thread local storage, and tries to be efficient about when to make I/O calls.

## Run and print I/O call usage

```
$ cargo run
    Finished dev [unoptimized + debuginfo] target(s) in 0.02s
     Running `target/debug/rust-async-bench`
sync:  register=33 unregister=33 poll=34 accept=64 read=64 write=64
async: register=33 unregister=33 poll=34 accept=64 read=64 write=64
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

* The benchmarks test 32 requests. The difference between the async and non-async syscall benchmarks is 12.3us. Divided by 32, that's an overhead of around 385ns per request. In a server app, that's practically free. For comparison, `Box::new(mem::MaybeUninit::<[u8; 16384]>::uninit())` on the same machine takes 465ns.

## How it works

In order to simulate a somewhat realistic application style, the benchmark is implemented as a fake network server that responds to requests. It accepts "connections", which are bidirectional streams of bytes. For each connection, it reads a line of text as a request, then it writes a line of text as a response.

The I/O primitives are `FakeListener`, `FakeStream`, and `Poll`, analogous to `TcpListener`, `TcpStream`, and Mio's `Poll`. There is no client side, and thus no client-side overhead when benchmarking.

There are two kinds of tasks to perform: accept connections and process connections. The non-async version is implemented as a poll loop with all tasks intermingled. The async version is implemented as individual future instances for each task, that are then executed concurrently.

Both versions of the application can be run with or without syscalls. When syscalls are enabled, `libc::read` is called on an empty pipe every time there would have been an I/O operation.

It is relatively straightforward to write a single-threaded poll loop server that doesn't use heap allocations or thread local storage. Doing the same with async/await, and doing it without making extra syscalls, is a bit trickier. The following techniques are used in the async implementation:

* I/O objects register/unregister with the poller when they are initialized/dropped as opposed to when I/O futures are used. They also keep track of their readiness state at all times. This helps reduce the overhead of the I/O futures. For example, if a stream is known to be not readable and `read()` is called on it, the returned future will immediately return `Pending` when polled, without performing a syscall.

* The executor is generic over a single future type, `F`, and it stores the futures as non-boxed values. In order to support two kinds of tasks with only one future type, the accept handler and connection handler are implemented within the same async function, and the desired task is selected via argument. This way we can avoid heap allocations when spawning, at the cost of all the futures taking up the same amount of memory.

* The waker points at a struct that is known not to move for the lifetime of a future, and this struct contains references to the associated executor and task. This enables the waker to find the executor and the task it is responsible for, without having to do any heap allocations on its own or use thread local storage to find the executor. For this to be safe, a waker (or more specifically the underlying shared data of a waker, as a waker can be cloned) must not outlive the future it was created for. This is a pretty reasonable condition to adhere to, and the executor asserts it at runtime whenever a future completes.

* Lifetime annotations everywhere! There is no `Rc` used, and all shared objects are passed along as references. The reactor must live as long as the executor and the I/O objects, the executor must live as long as the top-level futures, the top-level futures must live as long as the I/O objects, and the I/O objects must live as long as the I/O futures. Somehow it all works. The Rust compiler is amazing.

Further notes:

* The waker concept seems to exist to enable decoupling task execution from I/O, and it is possible to implement an executor without a separate reactor object or using wakers. However, the implementation uses wakers anyway, as it is how Rust async/await is intended to work.

* A global executor would be reasonable, and would make it easier to manage the safety around waker lifetimes. However, it's unclear how an executor of a generic `F` would be instantiated as a global variable, when `F` is an anonymous future.
