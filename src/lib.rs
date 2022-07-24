pub mod executor;
pub mod fakeio;
pub mod future;
pub mod list;
pub mod run;

use crate::fakeio::Stats;
use crate::run::{run_async, run_manual};

pub fn run() {
    let stats = Stats::new(true);
    run_manual(&stats);
    println!("sync:  {}", stats);

    let stats = Stats::new(true);
    run_async(&stats);
    println!("async: {}", stats);
}
