use std::marker::PhantomData;
use std::cell::Cell;
use std::sync::atomic::AtomicUsize;
use std::sync::{Mutex, Condvar, Arc};
use std::time::{Duration, Instant};
use std::sync::atomic::Ordering::SeqCst;
use std::fmt::Formatter;

pub fn pair() -> (Parker, Unparker) {
    let p = Parker::new();
    let u = p.unparker();
    (p, u)
}

/// Waits for a notification
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

    /// Blocks until notified and then goes back into unnotified state
    pub fn park(&self) {
        self.unparker.inner.park(None);
    }

    /// Blocks until notified and then goes back into unnotified state, or times out after `duration`
    ///
    /// return `true` if notified before the timeout
    pub fn park_timeout(&self, duration: Duration) -> bool {
        self.unparker.inner.park(Some(duration))
    }

    /// Blocks until notified and then goes back into unnotified state, or times out at `instant`
    ///
    /// return `true` if notified before the deadline
    pub fn park_deadline(&self, instant: Instant) -> bool {
        self.unparker.inner.park(Some(instant.saturating_duration_since(Instant::now())))
    }

    /// Notifies the parker
    ///
    /// return `true` if this call is the first to notify the parker, or `false`
    /// if the parker was already notified
    pub fn unpark(&self) -> bool {
        self.unparker.unpark()
    }

    /// Return a handle for unparking
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
                    // Block the current thread on the conditional variable
                    m = self.cvar.wait(m).unwrap();
                    if self.state.compare_exchange(NOTIFIED, EMPTY, SeqCst, SeqCst).is_ok() {
                        // got a notification
                        return true;
                    }
                }
            }
            Some(timeout) => {
                // Wait with a timeout, and if we spuriously wake up or otherwise wake up from a notification we just want to
                // unconditionally set `state` back to `EMPTY`, either consuming a notification or un-flagging ourselves as parked
                let (_m, _result) = self.cvar.wait_timeout(m, timeout).unwrap();
                match self.state.swap(EMPTY, SeqCst) {
                    NOTIFIED => true,  // got a notification
                    PARKED => false,   // no notification
                    n => panic!("inconsistent park_timeout state: {}", n)
                }
            }
        }
    }

    pub fn unpark(&self) -> bool {
        // To ensure the unparked thread will observe any writes we made before this call, we must
        // perform a release operation that `park` can synchronize with. To do that we must write
        // `NOTIFIED` even if `state` is already `NOTIFIED`. That is why this must be a swap rather
        // than a compare-and-swap that returns if it reads `NOTIFIED` on failure.
        match self.state.swap(NOTIFIED, SeqCst) {
            EMPTY => return true,      // no one was waiting
            NOTIFIED => return false,  // already unparked
            PARKED => {},              // gotta go wake someone up
            _ => panic!("inconsistent state in unpark")
        }

        // There is a period between when the parked thread sets `state` to `PARKED` (or last
        // checked `state` in the case of a spurious wakeup) and when it actually waits on `cvar`.
        // If we were to notify during this period it would be ignored and then when the parked
        // thread went to sleep it would never wake up. Fortunately, it has `lock` locked at this
        // stage so we can acquire `lock` to wait until it is ready to receive the notification.
        //
        // Releasing `lock` before the call to `notify_one` means that when the parked thread wakes
        // it doesn't get woken only to have to wait for us to release `lock`.
        drop(self.lock.lock().unwrap());
        self.cvar.notify_one();
        true
    }
}