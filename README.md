# Rust Async Benchmark

This project attempts to compare the performance of a manually written poll loop vs async/await. It uses "fake" I/O objects that work in memory. The async executor uses no allocs, mutexes, or thread local storage, and tries to be efficient about when to make I/O calls.

## Run and print I/O call usage

```
$ cargo run
    Finished dev [unoptimized + debuginfo] target(s) in 0.02s
     Running `target/debug/rust-async-bench`
sync: StatsData { register: 11, unregister: 11, poll: 12, accept: 20, read: 20, write: 20 }
async: StatsData { register: 11, unregister: 11, poll: 12, accept: 20, read: 20, write: 20 }
```

## Run benchmark

```
cargo bench
```

Some results:

| Function  | Time     |
| --------- | -------- |
| run_sync  | 1.2315us |
| run_async | 5.9816us |
