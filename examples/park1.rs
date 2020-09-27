use std::thread;
use std::time::Duration;

fn main() {
    let (p, u) = parking::pair();

    // Notify the parker
    assert_eq!(u.unpark(), true);

    // Wakes up immediately because the parker is notified
    p.park();

    thread::spawn(move || {
        thread::sleep(Duration::from_millis(500));
        u.unpark();
    });

    p.park();

}