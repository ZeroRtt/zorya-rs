use std::sync::atomic::{AtomicUsize, Ordering};

/// A mio token generator.
#[derive(Default)]
pub(crate) struct TokenGenerator(AtomicUsize);

impl TokenGenerator {
    pub(crate) fn next(&self) -> usize {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}
