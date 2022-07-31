use criterion::{criterion_group, criterion_main, Criterion};
use rust_async_bench::run;

fn criterion_benchmark(c: &mut Criterion) {
    run::run_manual(false, |r| {
        c.bench_function("manual", |b| b.iter(|| r()));
    });

    run::run_nonbox(false, |r| {
        c.bench_function("nonbox", |b| b.iter(|| r()));
    });

    run::run_callerbox(false, |r| {
        c.bench_function("callerbox", |b| b.iter(|| r()));
    });

    run::run_large_nonbox(false, |r| {
        c.bench_function("large+nonbox", |b| b.iter(|| r()));
    });

    run::run_box(false, |r| {
        c.bench_function("box", |b| b.iter(|| r()));
    });

    run::run_box_callerbox(false, |r| {
        c.bench_function("box+callerbox", |b| b.iter(|| r()));
    });

    run::run_large_box(false, |r| {
        c.bench_function("large+box", |b| b.iter(|| r()));
    });

    run::run_box_rc(false, run::BoxRcMode::RcWaker, |r| {
        c.bench_function("box+rc", |b| b.iter(|| r()));
    });

    run::run_box_rc(false, run::BoxRcMode::CheckedRcWaker, |r| {
        c.bench_function("box+chkrc", |b| b.iter(|| r()));
    });

    run::run_box_rc(false, run::BoxRcMode::ArcWaker, |r| {
        c.bench_function("box+arc", |b| b.iter(|| r()));
    });

    run::run_manual(true, |r| {
        c.bench_function("manual+syscalls", |b| b.iter(|| r()));
    });

    run::run_nonbox(true, |r| {
        c.bench_function("nonbox+syscalls", |b| b.iter(|| r()));
    });

    run::run_box(true, |r| {
        c.bench_function("box+syscalls", |b| b.iter(|| r()));
    });

    run::run_box_rc(true, run::BoxRcMode::RcWaker, |r| {
        c.bench_function("box+rc+syscalls", |b| b.iter(|| r()));
    });

    run::run_box_rc(true, run::BoxRcMode::CheckedRcWaker, |r| {
        c.bench_function("box+chkrc+syscalls", |b| b.iter(|| r()));
    });

    run::run_box_rc(true, run::BoxRcMode::ArcWaker, |r| {
        c.bench_function("box+arc+syscalls", |b| b.iter(|| r()));
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
