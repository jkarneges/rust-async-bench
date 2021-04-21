use criterion::{criterion_group, criterion_main, Criterion};
use rust_async_bench::fakeio::Stats;
use rust_async_bench::run::{run_async, run_async_frs, run_sync, AsyncPrealloc};
use std::rc::Rc;

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("run_sync", |b| {
        let stats = Rc::new(Stats::new(false));

        b.iter(|| {
            run_sync(&stats);
        })
    });

    c.bench_function("run_async", |b| {
        let prealloc = AsyncPrealloc::new(false);

        b.iter(|| {
            run_async(&prealloc);
        })
    });

    c.bench_function("run_async_frs", |b| {
        let prealloc = AsyncPrealloc::new(false);

        b.iter(|| {
            run_async_frs(&prealloc);
        })
    });

    c.bench_function("run_sync_with_syscalls", |b| {
        let stats = Rc::new(Stats::new(true));

        b.iter(|| {
            run_sync(&stats);
        })
    });

    c.bench_function("run_async_with_syscalls", |b| {
        let prealloc = AsyncPrealloc::new(true);

        b.iter(|| {
            run_async(&prealloc);
        })
    });

    c.bench_function("run_async_frs_with_syscalls", |b| {
        let prealloc = AsyncPrealloc::new(true);

        b.iter(|| {
            run_async_frs(&prealloc);
        })
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
