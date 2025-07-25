//! An improved hash time wheel algorithm based on binary heap priority queue.
//!
//! Unlike the classic [`Hashed and Hierarchical Timing Wheels`], this library uses a
//! binary heap as a priority queue for timers.
//! During [`spin`], it only needs to compare the timer ticks at the head of the
//! heap to quickly detect expired timers.
//!
//! [`Hashed and Hierarchical Timing Wheels`]: https://dl.acm.org/doi/pdf/10.1145/41457.37504

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap},
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

type Slot = Reverse<u64>;

struct TimeWheelImpl<T> {
    /// ticks
    ticks: u64,
    /// slot queue.
    priority_queue: BinaryHeap<Slot>,
    /// timers.
    timers: HashMap<u64, Vec<T>>,
    /// alive timer counter.
    counter: usize,
}

impl<T> TimeWheelImpl<T> {
    fn new() -> Self {
        Self {
            ticks: 0,
            priority_queue: Default::default(),
            timers: Default::default(),
            counter: 0,
        }
    }
}

/// A binary-heap based timing wheel implementation.
pub struct TimeWheel<T> {
    /// The timewheel start timestamp.
    start: Instant,
    /// timewheel tick interval in secs.
    tick_interval: u64,
    /// sync data.
    sys: Mutex<TimeWheelImpl<T>>,
    /// cond variable.
    cond: Condvar,
}

impl<T> TimeWheel<T> {
    /// Create a time-wheel with minimum time interval resolution `tick_interval`.
    pub fn new(tick_interval: Duration) -> Self {
        Self {
            start: Instant::now(),
            tick_interval: tick_interval.as_micros() as u64,
            sys: Mutex::new(TimeWheelImpl::new()),
            cond: Condvar::new(),
        }
    }

    /// Create new deadline.
    ///
    /// Returns tick number, if success.
    ///
    /// Returns `None`, if the deadline is already expired.
    pub fn deadline(&self, deadline: Instant, value: T) -> Option<u64> {
        let duration = (deadline - self.start).as_micros() as u64;

        let mut ticks = duration / self.tick_interval;

        if duration % self.tick_interval != 0 {
            ticks += 1;
        }

        let mut sys = self.sys.lock().unwrap();

        if !(ticks > sys.ticks) {
            log::trace!(
                "deadline: timer is already expired, ticks={}, process={}",
                ticks,
                sys.ticks
            );

            return None;
        }

        log::trace!(
            "deadline: create timer, ticks={}, process={}",
            ticks,
            sys.ticks
        );

        if let Some(timers) = sys.timers.get_mut(&ticks) {
            timers.push(value);
        } else {
            sys.timers.insert(ticks, vec![value]);
            sys.priority_queue.push(Reverse(ticks));
        }

        sys.counter += 1;

        self.cond.notify_one();

        Some(ticks)
    }

    /// Poll timeout timers.
    pub fn poll(&self, events: &mut Vec<T>) {
        let len = events.len();

        let mut sys = self.sys.lock().unwrap();

        loop {
            let interval = (Instant::now() - self.start).as_micros() as u64;

            sys.ticks = interval / self.tick_interval;

            let mut duration = None;

            while let Some(slot) = sys.priority_queue.peek() {
                if slot.0 > sys.ticks {
                    duration = Some(Duration::from_micros(
                        (slot.0 - sys.ticks) * self.tick_interval,
                    ));

                    break;
                }

                let slot = sys.priority_queue.pop().unwrap().0;

                if let Some(mut timers) = sys.timers.remove(&slot) {
                    sys.counter -= timers.len();

                    events.append(&mut timers);
                }
            }

            if events.len() > len {
                return;
            }

            if let Some(duration) = duration {
                log::trace!("poll: wait timeout {:?}", duration);
                (sys, _) = self.cond.wait_timeout(sys, duration).unwrap();
            } else {
                sys = self.cond.wait(sys).unwrap();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, mpsc},
        thread::spawn,
        time::{Duration, Instant},
    };

    use crate::TimeWheel;

    #[test]
    fn test_already_expired() {
        let time_wheel = TimeWheel::<()>::new(Duration::from_millis(200));

        assert!(
            time_wheel
                .deadline(Instant::now() - Duration::from_millis(200), ())
                .is_none()
        );
    }

    #[test]
    fn test_poll_notify() {
        let time_wheel = Arc::new(TimeWheel::<()>::new(Duration::from_millis(200)));

        let time_wheel_cloned = time_wheel.clone();

        let (sender, receiver) = mpsc::channel();

        spawn(move || {
            let mut events = vec![];
            time_wheel_cloned.poll(&mut events);
            sender.send(events).unwrap();
        });

        assert!(receiver.try_recv().is_err());

        assert!(time_wheel.deadline(Instant::now(), ()).is_some());

        assert_eq!(receiver.recv().unwrap(), vec![()]);
    }

    #[test]
    fn test_wait() {
        let time_wheel = Arc::new(TimeWheel::<()>::new(Duration::from_millis(200)));

        let time_wheel_cloned = time_wheel.clone();

        assert!(time_wheel.deadline(Instant::now(), ()).is_some());

        let (sender, receiver) = mpsc::channel();

        spawn(move || {
            let mut events = vec![];
            time_wheel_cloned.poll(&mut events);
            sender.send(events).unwrap();

            let mut events = vec![];
            time_wheel_cloned.poll(&mut events);
            sender.send(events).unwrap();
        });

        assert_eq!(receiver.recv().unwrap(), vec![()]);

        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn test_wait2() {
        let time_wheel = Arc::new(TimeWheel::<()>::new(Duration::from_millis(200)));

        assert!(
            time_wheel
                .deadline(Instant::now() + Duration::from_millis(100), ())
                .is_some()
        );

        assert!(
            time_wheel
                .deadline(Instant::now() + Duration::from_millis(200), ())
                .is_some()
        );

        let mut events = vec![];
        time_wheel.poll(&mut events);

        assert_eq!(events, vec![()]);

        time_wheel.poll(&mut events);

        assert_eq!(events, vec![(), ()]);
    }
}
