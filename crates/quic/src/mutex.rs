use std::sync;

#[allow(unused)]
pub struct Mutex<T: ?Sized> {
    sys: sync::Mutex<T>,
}
