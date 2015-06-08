use super::monovec::{MonoVec, Chunks};
use std::marker::{self, PhantomData};
use std::ops;
use std::mem;
use std::ptr;
use std::intrinsics;
use std::cell::Cell;

const SENTINEL: usize = !0;

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

struct Forward<E: ?Sized> {
    // Pointer to object
    obj: *mut u8,
    // Pointer past end of object
    end: *mut u8,
    // Convert to erased type
    erase: unsafe fn(*mut u8) -> *const E,
    // Drop glue
    drop: unsafe fn(*mut u8),
    // Backward function
    backward: BackwardFn<E>
}

// Function that returns a Forward structure
// One of these is stored prior to each object
// in the vector.
type ForwardFn<E> = unsafe fn(*mut FencePost<E>) -> Forward<E>;

struct Backward<E: ?Sized> {
    forward: ForwardFn<E>,
    fence: *mut FencePost<E>
}

type BackwardFn<E> = unsafe fn(*mut u8) -> Backward<E>;

struct FencePost<E: ?Sized> {
    word: usize,
    _ph: PhantomData<E>
}

impl<E: ?Sized> FencePost<E> {
    fn new(f: ForwardFn<E>, b: BackwardFn<E>) -> Self {
        FencePost {
            word: f as usize ^ b as usize,
            _ph: PhantomData
        }
    }
    
    unsafe fn forward(&self, b: BackwardFn<E>) -> ForwardFn<E> {
        mem::transmute(self.word ^ b as usize)
    }
    
    unsafe fn backward(&self, f: ForwardFn<E>) -> BackwardFn<E> {
        mem::transmute(self.word ^ f as usize)
    }
}

pub struct HetVec<'gt, E: ?Sized, S=Unsize> {
    // The actual backing vector
    vec: MonoVec<u8>,
    // Most recent backward function
    backward: Cell<BackwardFn<E>>,
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
            backward: Cell::new(unsafe { mem::transmute(0usize) }),
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

    // Forward function for T
    unsafe fn forward<T>(fence: *mut FencePost<E>) -> Forward<E> where S: Erase<T, E> {
        unsafe fn drop<T>(it: *mut u8) {
            intrinsics::drop_in_place(it as *mut T);
        }

        unsafe fn erase<'a, T:'a, EI: ?Sized, SI>(it: *mut u8) -> *const EI
                where SI: Erase<T, EI> {
            SI::erase(&*(it as *mut T)) as *const EI
        }

        let obj = fence.offset(1).align_for::<T>() as *mut u8;
        let end = obj.offset(mem::size_of::<T>() as isize);

        Forward {
            obj: obj,
            end: end,
            drop: drop::<T>,
            erase: erase::<T, E, S>,
            backward: Self::backward::<T>
        }
    }
    
    // Backward function for T
    unsafe fn backward<T>(end: *mut u8) -> Backward<E> where S: Erase<T, E> {
        let mut ptr = end.offset(-(mem::size_of::<T>() as isize));
        
        if mem::min_align_of::<T>() <= mem::min_align_of::<FencePost<E>>() {
            ptr = (ptr as usize & !(mem::min_align_of::<FencePost<E>>() - 1)) as *mut u8;
            ptr = ptr.offset(-(mem::size_of::<FencePost<E>>() as isize));
        } else {
            ptr = ptr.offset(-(mem::size_of::<FencePost<E>>() as isize));
            while *(ptr as *mut usize) ^ Self::forward::<T> as usize == SENTINEL {
                ptr = ptr.offset(-(mem::min_align_of::<usize>() as isize))
            }
        }

        Backward {
            forward: Self::forward::<T>,
            fence: ptr as *mut FencePost<E>
        }
    }

    unsafe fn alloc<T>(&self) -> *mut T where S: Erase<T, E> {
        let size = Self::space_for::<FencePost<E>>() + Self::space_for::<T>();
        let (space, _) = self.vec.reserve(size);
        let fence = space.align_for::<FencePost<E>>() as *mut FencePost<E>;
        let obj = fence.offset(1).align_for::<T>() as *mut T;
        self.vec.add_len(obj.offset(1).diff(space) as usize);
        // Fill padding with sentinel value
        let mut sentinel = fence.offset(1) as *mut usize;
        while sentinel != obj as *mut usize {
            *sentinel = Self::forward::<T> as usize ^ SENTINEL;
            sentinel = sentinel.offset(mem::size_of::<usize>() as isize);
        }
        *fence = FencePost::new(Self::forward::<T>, self.backward.get());
        obj
    }

    pub fn push<T:'gt>(&self, elem: T) -> &T where S: Erase<T, E> {
        unsafe {
            let obj = self.alloc::<T>();
            ptr::write(obj, elem);
            self.backward.set(Self::backward::<T>);
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
            back_cur: ptr::null_mut(),
            back_start: ptr::null_mut(),
            backward: unsafe { mem::transmute(0usize) },
            back_backward: self.backward.get(),
            _ph: PhantomData
        }
    }
}

impl<'gt, E: ?Sized, S> Drop for HetVec<'gt, E, S> {
    fn drop(&mut self) {
        unsafe {
            let mut backward = mem::transmute(0usize);
        
            for chunk in self.vec.chunks() {
                let mut cur = chunk.as_ptr() as *mut u8;
                let end = cur.offset(chunk.len() as isize);
                while cur != end {
                    let fence = cur.align_for::<FencePost<E>>() as *mut FencePost<E>;
                    let forward_fn = (*fence).forward(backward);
                    let forward = forward_fn(fence);
                    // Skip stub entries
                    if !forward.obj.is_null() {
                        (forward.drop)(forward.obj);
                    }
                    cur = forward.end;
                    backward = forward.backward;
                }
            }
        }
    }
}

pub struct Items<'a, E: ?Sized> {
    chunks: Chunks<'a, u8>,
    cur: *mut u8,
    end: *mut u8,
    back_cur: *mut u8,
    back_start: *mut u8,
    backward: BackwardFn<E>,
    back_backward: BackwardFn<E>,
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
                        None => { 
                            if self.back_start.is_null() {
                                return None
                            } else {
                                self.cur = self.back_start;
                                self.end = self.back_cur;
                            }
                        }
                    }
                }

                let fence = self.cur.align_for::<FencePost<E>>() as *mut FencePost<E>;
                let forward_fn = (*fence).forward(self.backward);
                let forward = forward_fn(fence);
                if self.back_start == self.cur {
                    self.back_start = forward.end
                }
                self.cur = forward.end;
                self.backward = forward.backward;
                // Skip stub entries
                if !forward.obj.is_null() {
                    return Some(&*(forward.erase)(forward.obj))
                }
            }
        }
    }
}

impl<'a, E: ?Sized> DoubleEndedIterator for Items<'a, E> {
    fn next_back(&mut self) -> Option<&'a E> {
        loop {
            unsafe {
                while self.back_cur == self.back_start {
                    match self.chunks.next_back() {
                        Some(s) => {
                            self.back_start = s.as_ptr() as *mut u8;
                            self.back_cur = self.back_start.offset(s.len() as isize);
                        }
                        None => {
                            if self.end.is_null() {
                                return None 
                            } else {
                                self.back_cur = self.end;
                                self.back_start = self.cur;
                            }
                        }
                    }
                }

                let backward = (self.back_backward)(self.back_cur);
                let forward = (backward.forward)(backward.fence);
                if self.end == self.back_cur {
                    self.end = backward.fence as *mut u8;
                }
                self.back_cur = backward.fence as *mut u8;
                self.back_backward = (*backward.fence).backward(backward.forward);
                // Skip stub entries
                if !forward.obj.is_null() {
                    return Some(&*(forward.erase)(forward.obj))
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

        for item in vec.into_iter().rev() {
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
