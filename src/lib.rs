pub mod list;
pub mod run;

mod executor;
mod fakeio;
mod future;
mod waker;

pub fn run() {
    println!("manual: {}", crate::run::run_manual(true, |r| r()));
    println!("async: {}", crate::run::run_nonbox(true, |r| r()));
}
