extern crate coio;

use std::sync::Arc;

fn invoke_every_ms<F>(ms: u64, f: F)
    where F: Fn() + Sync + Send + 'static
{
    let f = Arc::new(f);
    coio::spawn(move|| {
        loop {
            coio::sleep_ms(ms);
            let f = f.clone();
            coio::spawn(move|| (*f)());
        }
    });
}

fn main() {
    coio::spawn(|| {
        for i in 0..10 {
            println!("Qua  {}", i);
            coio::sleep_ms(2000);
        }
    });

    invoke_every_ms(1000, || println!("Purr :P"));

    coio::run(2);
}
