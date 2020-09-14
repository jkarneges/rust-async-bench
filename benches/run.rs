use criterion::{criterion_group, criterion_main, Criterion};
use rust_async_bench::fakeio::Stats;
use rust_async_bench::run::{run_async, run_sync};

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("run_sync", |b| {
        b.iter(|| {
            let stats = Stats::new(false);
            run_sync(&stats);
        })
    });

    c.bench_function("run_async", |b| {
        b.iter(|| {
            let stats = Stats::new(false);
            run_async(&stats);
        })
    });

    c.bench_function("run_sync_with_syscalls", |b| {
        b.iter(|| {
            let stats = Stats::new(true);
            run_sync(&stats);
        })
    });

    c.bench_function("run_async_with_syscalls", |b| {
        b.iter(|| {
            let stats = Stats::new(true);
            run_async(&stats);
        })
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
