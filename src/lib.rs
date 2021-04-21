pub mod fakeio;
pub mod future;
pub mod list;
pub mod run;

use crate::fakeio::Stats;
use crate::run::{run_async, run_async_frs, run_sync, AsyncPrealloc};
use std::rc::Rc;

pub fn run() {
    let stats = Rc::new(Stats::new(true));
    run_sync(&stats);
    println!("sync:  {}", stats);

    let prealloc = AsyncPrealloc::new(true);
    run_async(&prealloc);
    println!("async: {}", prealloc.stats);

    let prealloc = AsyncPrealloc::new(true);
    run_async_frs(&prealloc);
    println!("async_frs: {}", prealloc.stats);
}
