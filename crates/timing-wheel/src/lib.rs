//! An improved hash time wheel algorithm based on binary heap priority queue.
//!
//! Unlike the classic [`Hashed and Hierarchical Timing Wheels`], this library uses a
//! binary heap as a priority queue for timers.
//! During [`spin`], it only needs to compare the timer ticks at the head of the
//! heap to quickly detect expired timers.
//!
//! # Examples
//!
//! ```
//! use std::time::{ Duration, Instant };
//! use timing_wheel::TimeWheel;
//! use std::thread::sleep;
//!
//! let mut time_wheel = TimeWheel::new(Duration::from_millis(1));
//!
//! time_wheel.deadline(Instant::now() + Duration::from_millis(1), ());
//!
//! sleep(Duration::from_millis(1));
//!
//! let mut wakers = vec![];
//!
//! time_wheel.spin(&mut wakers);
//!
//! assert_eq!(wakers, vec![()]);
//! ```
//!
//! [`spin`]: TimeWheel::spin
//! [`Hashed and Hierarchical Timing Wheels`]: https://dl.acm.org/doi/pdf/10.1145/41457.37504

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap},
    time::{Duration, Instant},
};

type Slot = Reverse<u64>;

/// A binary-heap based timing wheel implementation.
pub struct TimeWheel<T> {
    /// The timewheel start timestamp.
    start: Instant,
    /// timewheel tick interval in secs.
    tick_interval: u64,
    /// ticks
    ticks: u64,
    /// slot queue.
    priority_queue: BinaryHeap<Slot>,
    /// timers.
    timers: HashMap<u64, Vec<T>>,
    /// alive timer counter.
    counter: usize,
}

impl<T> TimeWheel<T> {
    /// Create a time-wheel with minimum time interval resolution `tick_interval`.
    pub fn new(tick_interval: Duration) -> Self {
        Self {
            tick_interval: tick_interval.as_micros() as u64,
            ticks: 0,
            start: Instant::now(),
            priority_queue: Default::default(),
            timers: Default::default(),
            counter: 0,
        }
    }

    /// Returns the number of alive timers.
    pub fn len(&self) -> usize {
        self.counter
    }

    /// Create a new timer using provided `deadline`.
    ///
    /// Return `None` if the deadline is already reach.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::{ Duration, Instant };
    /// use timing_wheel::TimeWheel;
    /// use std::thread::sleep;
    ///
    /// let mut time_wheel = TimeWheel::new(Duration::from_millis(1));
    ///
    /// time_wheel.deadline(Instant::now() + Duration::from_millis(1), ());
    ///
    /// sleep(Duration::from_millis(1));
    ///
    /// let mut wakers = vec![];
    ///
    /// time_wheel.spin(&mut wakers);
    ///
    /// assert_eq!(wakers, vec![()]);
    /// ```
    pub fn deadline(&mut self, deadline: Instant, value: T) -> Option<u64> {
        let ticks = (deadline - self.start).as_micros() as u64 / self.tick_interval;

        if !(ticks > self.ticks) {
            return None;
        }

        if let Some(timers) = self.timers.get_mut(&ticks) {
            timers.push(value);
        } else {
            self.timers.insert(ticks, vec![value]);
            self.priority_queue.push(Reverse(ticks));
        }

        self.counter += 1;

        Some(ticks)
    }

    /// Create a new `deadline` with a value equal to `Instant::now() + duration`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use timing_wheel::TimeWheel;
    /// use std::thread::sleep;
    ///
    /// let mut time_wheel = TimeWheel::new(Duration::from_millis(1));
    ///
    /// time_wheel.after(Duration::from_millis(1), ());
    ///
    /// sleep(Duration::from_millis(1));
    ///
    /// let mut wakers = vec![];
    ///
    /// time_wheel.spin(&mut wakers);
    ///
    /// assert_eq!(wakers, vec![()]);
    /// ```
    pub fn after(&mut self, duration: Duration, value: T) -> Option<u64> {
        self.deadline(Instant::now() + duration, value)
    }

    //// Spin the wheel according to the current time
    pub fn spin(&mut self, wakers: &mut Vec<T>) {
        let to_slot = (Instant::now() - self.start).as_micros() as u64 / self.tick_interval;

        while let Some(slot) = self.priority_queue.peek() {
            if slot.0 > to_slot {
                break;
            }

            let slot = self.priority_queue.pop().unwrap().0;

            if let Some(mut timers) = self.timers.remove(&slot) {
                self.counter -= timers.len();
                wakers.append(&mut timers);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{thread::sleep, time::Duration};

    use super::*;

    #[test]
    fn test_len() {
        let mut time_wheel = TimeWheel::new(Duration::from_millis(1));

        time_wheel
            .deadline(time_wheel.start + Duration::from_millis(1), ())
            .expect("deadline is valid.");

        assert_eq!(time_wheel.len(), 1);

        sleep(Duration::from_millis(1));

        let mut wakers = vec![];

        time_wheel.spin(&mut wakers);

        assert_eq!(wakers, vec![()]);
    }

    #[test]
    fn test_order() {
        let mut time_wheel = TimeWheel::new(Duration::from_millis(1));

        let deadline = time_wheel.start + Duration::from_millis(1);

        time_wheel
            .deadline(deadline, 1)
            .expect("deadline is valid.");

        time_wheel
            .deadline(deadline, 2)
            .expect("deadline is valid.");

        sleep(Duration::from_millis(1));

        let mut wakers = vec![];

        time_wheel.spin(&mut wakers);

        assert_eq!(wakers, vec![1, 2]);
    }

    #[test]
    fn test_order2() {
        let mut time_wheel = TimeWheel::new(Duration::from_millis(1));

        time_wheel
            .deadline(time_wheel.start + Duration::from_millis(1), 1)
            .expect("deadline is valid.");

        time_wheel
            .deadline(time_wheel.start + Duration::from_millis(2), 2)
            .expect("deadline is valid.");

        assert_eq!(time_wheel.len(), 2);

        sleep(Duration::from_millis(1));

        let mut wakers = vec![];

        time_wheel.spin(&mut wakers);

        assert_eq!(wakers, vec![1]);

        assert_eq!(time_wheel.len(), 1);

        sleep(Duration::from_millis(1));

        time_wheel.spin(&mut wakers);

        assert_eq!(wakers, vec![1, 2]);

        assert_eq!(time_wheel.len(), 0);
    }

    #[test]
    fn test_after() {
        let mut time_wheel = TimeWheel::new(Duration::from_millis(1));

        time_wheel.after(Duration::from_millis(1), ());

        sleep(Duration::from_millis(1));

        let mut wakers = vec![];

        time_wheel.spin(&mut wakers);

        assert_eq!(wakers, vec![()]);
    }
}
