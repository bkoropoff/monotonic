use super::monovec::MonoVec;
use super::hetvec::{Erase, HetVec};
use std::mem;

pub struct TypedArena<T> {
    vec: MonoVec<T>
}

impl<T> TypedArena<T> {
    #[allow(mutable_transmutes)]
    pub fn alloc<F: FnOnce() -> T>(&self, f: F) -> &mut T {
        unsafe { mem::transmute(self.vec.push(f())) }
    }

    pub fn with_capacity(count: usize) -> Self {
        TypedArena {
            vec: MonoVec::with_capacity(count)
        }
    }
}

// We don't permit iterating objects in the arena, so
// we use a trivial erase strategy
struct Forget;

impl<T> Erase<T, ()> for Forget {
    fn erase(_: &T) -> &() {
        static UNIT: () = ();
        &UNIT
    }
}

pub struct Arena<'gt> {
    vec: HetVec<'gt, (), Forget>
}

impl<'gt> Arena<'gt> {
    #[allow(mutable_transmutes)]
    pub fn alloc<T: 'gt, F: FnOnce() -> T>(&self, f: F) -> &mut T {
        unsafe { mem::transmute(self.vec.emplace(f)) }
    }
}
