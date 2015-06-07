use super::monovec::{MonoVec, Chunks};
use std::marker::{self, PhantomData};
use std::ops;
use std::mem;
use std::ptr;
use std::intrinsics;

pub trait Erase<T: ?Sized, E: ?Sized> {
    fn erase(real: &T) -> &E;
}

// Erasure strategy: coerce to unsized
pub struct Unsize(());

impl<T: ?Sized, E: ?Sized> Erase<T, E> for Unsize
        where T: marker::Unsize<E> {
    fn erase(real: &T) -> &E {
        real
    }
}

// Erasure strategy: deref
pub struct Deref(());

impl<T: ?Sized, E: ?Sized> Erase<T, E> for Deref
        where T: ops::Deref<Target=E> {
    fn erase(real: &T) -> &E {
        real
    }
}

struct Glue<E: ?Sized> {
    // Pointer to object
    obj: *mut u8,
    // Pointer past end of object
    end: *mut u8,
    // Convert to erased type
    erase: unsafe fn(*mut u8) -> *const E,
    // Drop glue
    drop: unsafe fn(*mut u8)
}

// Function that returns a glue structure
// One of these is stored prior to each object
// in the vector.
type GlueFn<E> = unsafe fn(*mut u8) -> Glue<E>;

pub struct HetVec<'gt, E: ?Sized, S=Unsize> {
    // The actual backing vector
    vec: MonoVec<u8>,
    // Indicate we contain E, ignore S,
    // and that 'gt must strictly outlive us
    _ph: PhantomData<(E, *const S, *mut &'gt ())>
}

unsafe impl<'gt, E: ?Sized + Send, S> Send for HetVec<'gt, E, S> {}

// Some utility methods for raw pointer
trait PtrUtil: Sized {
    unsafe fn align(self, a: usize) -> Self;
    fn as_u8_ptr(self) -> *mut u8;

    #[inline]
    unsafe fn align_for<T>(self) -> Self {
        self.align(mem::min_align_of::<T>())
    }

    #[inline]
    fn diff<O: PtrUtil>(self, o: O) -> isize {
        self.as_u8_ptr() as isize - o.as_u8_ptr() as isize
    }
}

impl<T> PtrUtil for *const T {
    #[inline]
    fn as_u8_ptr(self) -> *mut u8 {
        self as *mut u8
    }
    #[inline]
    unsafe fn align(self, a: usize) -> Self {
        let me = self as usize;
        (me.checked_add(a - 1).unwrap() & !(a - 1)) as Self
    }
}

impl<T> PtrUtil for *mut T {
    #[inline]
    fn as_u8_ptr(self) -> *mut u8 {
        self as *mut u8
    }
    #[inline]
    unsafe fn align(self, a: usize) -> Self {
        let me = self as usize;
        (me.checked_add(a - 1).unwrap() & !(a - 1)) as Self
    }
}

impl<'gt, E: ?Sized, S=Unsize> HetVec<'gt, E, S> {
    pub fn new() -> Self {
        Self::with_capacity(128)
    }

    pub fn with_capacity(cap: usize) -> Self {
        HetVec {
            vec: MonoVec::with_capacity(cap),
            _ph: PhantomData
        }
    }

    // Returns worst case space required to store something
    // in the vec with appropriate alignment.  This could be
    // improved to take the actual alignment of the vector
    // chunk offset into account.
    #[inline]
    fn space_for<T>() -> usize {
        mem::size_of::<T>() + mem::min_align_of::<T>() - 1
    }

    // Glue function for T
    unsafe fn glue<T>(data: *mut u8) -> Glue<E> where S: Erase<T, E> {
        unsafe fn drop<T>(it: *mut u8) {
            intrinsics::drop_in_place(it as *mut T);
        }

        unsafe fn erase<'a, T:'a, EI: ?Sized, SI>(it: *mut u8) -> *const EI
                where SI: Erase<T, EI> {
            SI::erase(&*(it as *mut T)) as *const EI
        }

        let obj = data.align_for::<T>();
        let end = obj.offset(mem::size_of::<T>() as isize);

        Glue {
            obj: obj,
            end: end,
            drop: drop::<T>,
            erase: erase::<T, E, S>
        }
    }

    unsafe fn alloc<T>(&self) -> (*mut GlueFn<E>, *mut T, usize) {
        let size = Self::space_for::<GlueFn<E>>() + Self::space_for::<T>();
        let (space, _) = self.vec.reserve(size);
        let glue = space.align_for::<GlueFn<E>>() as *mut GlueFn<E>;
        let obj = glue.offset(1).align_for::<T>() as *mut T;
        (glue, obj, obj.offset(1).diff(space) as usize)
    }

    pub fn push<T:'gt>(&self, elem: T) -> &T where S: Erase<T, E> {
        unsafe {
            let (glue, obj, len) = self.alloc::<T>();
            ptr::write(glue, Self::glue::<T>);
            ptr::write(obj, elem);
            self.vec.add_len(len);
            &*obj
        }
    }

    unsafe fn stub<T>(data: *mut u8) -> Glue<E> {
        Glue {
            obj: ptr::null_mut(),
            end: data.align_for::<T>().offset(mem::size_of::<T>() as isize),
            drop: mem::uninitialized(),
            erase: mem::uninitialized()
        }
    }

    pub fn emplace<T:'gt, F: FnOnce() -> T>(&self, f: F) -> &T
           where S: Erase<T, E> {
        unsafe {
            let (glue, obj, len) = self.alloc::<T>();
            // Write a stub glue function in case `f` panics
            ptr::write(glue, Self::stub::<T>);
            self.vec.add_len(len);
            ptr::write(obj, f());
            ptr::write(glue, Self::glue::<T>);
            &*obj
        }
    }
}

impl<'gt, 'a, E: ?Sized, S> IntoIterator for &'a HetVec<'gt, E, S> {
    type Item = &'a E;
    type IntoIter = Items<'a, E>;

    fn into_iter(self) -> Self::IntoIter {
        Items {
            chunks: self.vec.chunks(),
            cur: ptr::null_mut(),
            end: ptr::null_mut(),
            _ph: PhantomData
        }
    }
}

impl<'gt, E: ?Sized, S> Drop for HetVec<'gt, E, S> {
    fn drop(&mut self) {
        unsafe {
            for chunk in self.vec.chunks() {
                let mut cur = chunk.as_ptr() as *mut u8;
                let end = cur.offset(chunk.len() as isize);
                while cur != end {
                    let glue_fn = cur.align_for::<GlueFn<E>>() as *mut GlueFn<E>;
                    let glue = (*glue_fn)(glue_fn.offset(1) as *mut u8);
                    // Skip stub entries
                    if !glue.obj.is_null() {
                        (glue.drop)(glue.obj);
                    }
                    cur = glue.end;
                }
            }
        }
    }
}

pub struct Items<'a, E: ?Sized> {
    chunks: Chunks<'a, u8>,
    cur: *mut u8,
    end: *mut u8,
    _ph: PhantomData<E>
}

impl<'a, E: ?Sized> Iterator for Items<'a, E> {
    type Item = &'a E;

    fn next(&mut self) -> Option<&'a E> {
        loop {
            unsafe {
                while self.cur == self.end {
                    match self.chunks.next() {
                        Some(s) => {
                            self.cur = s.as_ptr() as *mut u8;
                            self.end = self.cur.offset(s.len() as isize);
                        }
                        None => return None
                    }
                }

                let glue_fn = self.cur.align_for::<GlueFn<E>>() as *mut GlueFn<E>;
                let glue = (*glue_fn)(glue_fn.offset(1) as *mut u8);
                self.cur = glue.end;
                // Skip stub entries
                if !glue.obj.is_null() {
                    return Some(&*(glue.erase)(glue.obj))
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::fmt::{self, Display};
    use std::str;

    #[test]
    fn unsize_trait() {
        struct Hi;
        impl Drop for Hi {
            fn drop(&mut self) { println!("DROPPED: {}", self); }
        }
        impl Display for Hi {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                "hello, world!".fmt(f)
            }
        }
                
        let vec: HetVec<Display> = HetVec::new();
        vec.push(42);
        vec.push("Weasel");
        vec.push(Hi);

        for item in &vec {
            println!("{}", item);
        }
    }

    #[test]
    fn unsize_slice() {
        let vec: HetVec<[u8]> = HetVec::new();
        vec.push(*b"hello");

        for item in &vec {
            println!("{}", str::from_utf8(item).unwrap());
        }
    }

    #[test]
    fn deref_str() {
        let vec: HetVec<str, Deref> = HetVec::new();
        vec.push(format!("Hello"));
        vec.push("world");

        for item in &vec {
            println!("{}", item);
        }
    }
}
