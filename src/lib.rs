pub mod fakeio;
pub mod future;
pub mod list;
pub mod run;

use crate::fakeio::Stats;
use crate::run::{run_async, run_sync};

pub fn run() {
    let stats = Stats::new();
    run_sync(&stats);
    println!("sync: {}", stats);

    let stats = Stats::new();
    run_async(&stats);
    println!("async: {}", stats);
}
