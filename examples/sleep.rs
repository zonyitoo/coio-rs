extern crate coio;

fn main() {
    coio::spawn(|| {
        for i in 0.. {
            println!("Qua  {}", i);
            coio::sleep_ms(2000);
        }
    });

    coio::spawn(|| {
        for i in 0.. {
            println!("Purr {}", i);
            coio::sleep_ms(1000);
        }
    });

    coio::run(2);
}
