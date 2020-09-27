use std::marker::PhantomData;
use std::cell::Cell;
use std::sync::atomic::AtomicUsize;
use std::sync::{Mutex, Condvar, Arc};
use std::time::{Duration, Instant};
use std::sync::atomic::Ordering::SeqCst;
use core::panicking::panic_fmt;
use std::fmt::Formatter;

pub struct Parker {
    unparker: Unparker,
    _marker: PhantomData<Cell<()>>
}

impl Parker {

    pub fn new() -> Parker {
        Parker {
            unparker: Unparker {
                inner: Arc::new(Inner {
                    state: AtomicUsize::new(EMPTY),
                    lock: Mutex::new(()),
                    cvar: Condvar::new()
                })
            },
            _marker: PhantomData
        }
    }

    pub fn park(&self) {
        self.unparker.inner.park(None);
    }

    pub fn park_timeout(&self, duration: Duration) -> bool {
        self.unparker.inner.park(Some(duration))
    }

    pub fn park_deadline(&self, instant: Instant) -> bool {
        self.unparker.inner.park(Some(instant.saturating_duration_since(Instant::now())))
    }

    pub fn unpark(&self) -> bool {
        self.unparker.unpark()
    }

    pub fn unparker(&self) -> Unparker {
        self.unparker.clone()
    }
}

impl Default for Parker {
    fn default() -> Self {
        Parker::new()
    }
}

impl std::fmt::Debug for Parker {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.pad("Parker { .. }")
    }
}

/// Notifies a parker
pub struct Unparker {
    inner: Arc<Inner>
}

impl Unparker {
    pub fn unpark(&self) -> bool {
        self.inner.unpark()
    }
}

impl std::fmt::Debug for Unparker {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.pad("Unparker { .. }")
    }
}

impl Clone for Unparker {
    fn clone(&self) -> Self {
        Unparker {
            inner: self.inner.clone()
        }
    }
}

const EMPTY: usize = 0;
const PARKED: usize = 1;
const NOTIFIED: usize = 2;

struct Inner {
    state: AtomicUsize,
    lock: Mutex<()>,
    cvar: Condvar
}

impl Inner {

    fn park(&self, timeout: Option<Duration>) -> bool {
        if self.state.compare_exchange(NOTIFIED, EMPTY, SeqCst, SeqCst).is_ok() {
            return true;
        }

        // If the timeout if zero, then there is no need to actually block
        if let Some(dur) = timeout {
            if dur == Duration::from_millis(0) {
                return false;
            }
        }

        // Otherwise we need to coordinate going to sleep
        let mut m = self.lock.lock().unwrap();

        match self.state.compare_exchange(EMPTY, PARKED, SeqCst, SeqCst) {
            Ok(_) => {},
            // Consume this notification to avoid spurious wakeups in the next park
            Err(NOTIFIED) => {
                let old = self.state.swap(EMPTY, SeqCst);
                assert_eq!(old, NOTIFIED, "park state changed unexpectedly");
                return true;
            }
            Err(n) => panic!("inconsistent park_timeout state: {}", n)
        }

        match timeout {
            None => {
                loop {
                    m = self.cvar.wait(m).unwrap();
                    if self.state.compare_exchange(NOTIFIED, EMPTY, SeqCst, SeqCst).is_ok() {
                        return true;
                    }
                }
            }
            Some(timeout) => {
                let (_m, _result) = self.cvar.wait_timeout(m, timeout).unwrap();
                match self.state.swap(EMPTY, SeqCst) {
                    NOTIFIED => true,
                    PARKED => false,
                    n => panic!("inconsistent park_timeout state: {}", n)
                }
            }
        }
    }

    pub fn unpark(&self) -> bool {
        match self.state.swap(NOTIFIED, SeqCst) {
            EMPTY => return true,
            NOTIFIED => return false,
            PARKED => {},
            _ => panic!("inconsistent state in unpark")
        }
        drop(self.lock.lock().unwrap());
        self.cvar.notify_one();
        true
    }
}