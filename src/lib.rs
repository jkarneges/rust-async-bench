pub mod executor;
pub mod future;
pub mod list;
pub mod run;

mod fakeio;
mod waker;

use crate::run::{run_manual, run_nonbox};

pub fn run() {
    println!("manual: {}", run_manual(true, |r| r()));
    println!("nonbox: {}", run_nonbox(true, |r| r()));
}
