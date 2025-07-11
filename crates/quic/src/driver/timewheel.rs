use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap},
    time::{Duration, Instant},
};

type Slot = Reverse<u64>;

/// A hashed timewheel alogrithem implemenation.
#[allow(unused)]
pub struct TimeWheel<T> {
    /// The timewheel start timestamp.
    start: Instant,
    /// timewheel tick interval in secs.
    tick_interval: u64,
    /// ticks
    ticks: u64,
    /// slot queue.
    priority_full_slot: BinaryHeap<Slot>,
    /// timers.
    timers: HashMap<u64, Vec<T>>,
    /// alive timer counter.
    counter: usize,
}

impl<T> TimeWheel<T> {
    /// Create a time-wheel with `tick_interval`
    pub fn new(tick_interval: Duration) -> Self {
        Self {
            tick_interval: tick_interval.as_micros() as u64,
            ticks: 0,
            start: Instant::now(),
            priority_full_slot: Default::default(),
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
    pub fn deadline(&mut self, deadline: Instant, value: T) -> Option<u64> {
        let ticks = (deadline - self.start).as_micros() as u64 / self.tick_interval;

        if !(ticks > self.ticks) {
            return None;
        }

        if let Some(timers) = self.timers.get_mut(&ticks) {
            timers.push(value);
        } else {
            self.timers.insert(ticks, vec![value]);
            self.priority_full_slot.push(Reverse(ticks));
        }

        self.counter += 1;

        Some(ticks)
    }

    //// Spin the wheel according to the current time
    pub fn spin(&mut self) -> Vec<T> {
        let to_slot = (Instant::now() - self.start).as_micros() as u64 / self.tick_interval;

        let mut wakers = vec![];

        while let Some(slot) = self.priority_full_slot.peek() {
            if slot.0 > to_slot {
                break;
            }

            let slot = self.priority_full_slot.pop().unwrap().0;

            if let Some(mut timers) = self.timers.remove(&slot) {
                self.counter -= timers.len();
                wakers.append(&mut timers);
            }
        }

        wakers
    }
}

#[cfg(test)]
mod tests {
    use std::{thread::sleep, time::Duration};

    use crate::driver::TimeWheel;

    #[test]
    fn test_len() {
        let mut time_wheel = TimeWheel::new(Duration::from_millis(1));

        time_wheel
            .deadline(time_wheel.start + Duration::from_millis(1), ())
            .expect("deadline is valid.");

        assert_eq!(time_wheel.len(), 1);

        sleep(Duration::from_millis(1));

        assert_eq!(time_wheel.spin(), vec![()]);
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

        assert_eq!(time_wheel.spin(), vec![1, 2]);
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

        sleep(Duration::from_millis(1));

        assert_eq!(time_wheel.spin(), vec![1]);

        sleep(Duration::from_millis(1));

        assert_eq!(time_wheel.spin(), vec![2]);
    }
}
