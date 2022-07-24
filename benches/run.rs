use criterion::{criterion_group, criterion_main, Criterion};
use rust_async_bench::executor::{Executor, Spawner};
use rust_async_bench::fakeio::Stats;
use rust_async_bench::future::FakeReactor;
use rust_async_bench::run::{do_async, AsyncInvoke, RunManual, CONNS_MAX};

fn criterion_benchmark(c: &mut Criterion) {
    {
        let stats = Stats::new(false);
        let mut r = RunManual::new(&stats);

        c.bench_function("manual", |b| b.iter(|| r.run()));
    }

    {
        let stats = Stats::new(false);
        let reactor = FakeReactor::new(CONNS_MAX + 1, &stats);
        let spawner = Spawner::new();
        let executor = Executor::new(&reactor, CONNS_MAX + 1, |invoke| {
            do_async(&spawner, &reactor, &stats, invoke)
        });
        executor.set_spawner(&spawner);

        c.bench_function("async", |b| {
            b.iter(|| {
                spawner.spawn(AsyncInvoke::Listen).unwrap();
                executor.exec();
            })
        });
    }

    {
        let stats = Stats::new(true);
        let mut r = RunManual::new(&stats);

        c.bench_function("manual+syscalls", |b| b.iter(|| r.run()));
    }

    {
        let stats = Stats::new(true);
        let reactor = FakeReactor::new(CONNS_MAX + 1, &stats);
        let spawner = Spawner::new();
        let executor = Executor::new(&reactor, CONNS_MAX + 1, |invoke| {
            do_async(&spawner, &reactor, &stats, invoke)
        });
        executor.set_spawner(&spawner);

        c.bench_function("async+syscalls", |b| {
            b.iter(|| {
                spawner.spawn(AsyncInvoke::Listen).unwrap();
                executor.exec();
            })
        });
    }
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
