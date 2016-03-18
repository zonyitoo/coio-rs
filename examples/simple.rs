extern crate coio;
extern crate env_logger;

use coio::Scheduler;

fn main() {
    env_logger::init().unwrap();

    Scheduler::new()
        .run(|| {
            for _ in 0..10 {
                println!("Heil Hydra");
                Scheduler::sched();
            }
        })
        .unwrap();
}
