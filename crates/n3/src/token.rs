//! mio token ID generation algorithm.

use std::{
    cell::RefCell,
    sync::atomic::{AtomicUsize, Ordering},
};

use mio::Token;

/// An mio token ID generation algorithm must implement this trait.
pub trait TokenNext {
    fn next(&self) -> Token;
}

/// A token generation algorithm for single-thread condition.
pub struct LocalTokenGenerator {
    counter: RefCell<usize>,
    step: usize,
}

impl LocalTokenGenerator {
    /// Create a new `LocalTokenGenerator` with id `start` value and id increment `step`.
    pub fn new(start: usize, step: usize) -> Self {
        Self {
            counter: RefCell::new(start),
            step,
        }
    }
}

/// Create a default `LocalTokenGenerator` with id start value `0` and id increment step `1`.
impl Default for LocalTokenGenerator {
    fn default() -> Self {
        Self::new(0, 1)
    }
}

impl TokenNext for LocalTokenGenerator {
    fn next(&self) -> Token {
        let token = Token(*self.counter.borrow());
        (*self.counter.borrow_mut(), _) = token.0.overflowing_add(self.step);
        token
    }
}

/// A token generation algorithm for multi-thread condition.
pub struct MultiThreadTokenGenerator {
    counter: AtomicUsize,
    step: usize,
}

impl MultiThreadTokenGenerator {
    /// Create a new `MultiThreadTokenGenerator` with id `start` value and id increment `step`.
    pub fn new(start: usize, step: usize) -> Self {
        Self {
            counter: AtomicUsize::new(start),
            step,
        }
    }
}

/// Create a default `MultiThreadTokenGenerator` with id start value `0` and id increment step `1`.
impl Default for MultiThreadTokenGenerator {
    fn default() -> Self {
        Self::new(0, 1)
    }
}

impl TokenNext for MultiThreadTokenGenerator {
    fn next(&self) -> Token {
        Token(self.counter.fetch_add(self.step, Ordering::SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, thread::spawn};

    use crate::token::{MultiThreadTokenGenerator, TokenNext};

    #[test]
    fn test_multi_thread_gen() {
        let generator: Arc<Box<dyn TokenNext + Send + Sync>> =
            Arc::new(Box::new(MultiThreadTokenGenerator::default()));

        let gen1 = generator.clone();

        _ = spawn(move || gen1.next()).join();

        generator.next();
    }
}
