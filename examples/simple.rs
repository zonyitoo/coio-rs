extern crate coio;

use coio::Scheduler;

fn main() {
    Scheduler::spawn(|| {
        for _ in 0..10 {
            println!("Heil Hydra");
        }
    });

    Scheduler::run(1);
}
