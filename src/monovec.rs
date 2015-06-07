use std::fmt;
use std::mem;
use std::ptr;
use std::slice;
use std::iter;
use std::io::{self, Write};
use std::cmp;
use std::str;
use std::rt::heap;
use std::cell::Cell;
use std::marker::PhantomData;
use std::intrinsics;

// A chunk in the vector
struct Chunk<T> {
    // Previous chunk
    prev: *mut Chunk<T>,
    // Next chunk
    next: *mut Chunk<T>,
    // Count of items
    len: usize,
    // Capacity
    cap: usize,
    // Items follow in memory
    items: [T; 0]
}

pub struct MonoVec<T> {
    head: Cell<*mut Chunk<T>>,
    tail: Cell<*mut Chunk<T>>,
    _ph: PhantomData<T>
}

unsafe impl<T: Send> Send for MonoVec<T> {}

impl<T> Chunk<T> {
    fn array_size(len: usize) -> usize {
        len.checked_mul(mem::size_of::<T>()).unwrap()
    }
    
    fn mem_size(len: usize) -> usize {
        mem::size_of::<Self>().checked_add(Self::array_size(len)).unwrap()
    }
    
    fn new(cap: usize) -> *mut Self {
        unsafe {
            let res = heap::allocate(Self::mem_size(cap),
                                     mem::align_of::<Self>()) as *mut Self;
            ptr::write(&mut (*res).prev, ptr::null_mut());
            ptr::write(&mut (*res).next, ptr::null_mut());
            ptr::write(&mut (*res).len, 0);
            ptr::write(&mut (*res).cap, cap);
            res
        }
    }
}

impl<T> MonoVec<T> {
    pub fn new() -> Self {
        Self::with_capacity(8)
    }
    
    pub fn with_capacity(cap: usize) -> Self {
        let head = Chunk::new(cmp::max(cap, 1));
        MonoVec {
            head: Cell::new(head),
            tail: Cell::new(head),
            _ph: PhantomData
        }
    }

    // FIXME: track total len in header to make this O(1)?
    pub fn len(&self) -> usize {
        let mut len = 0;
        let mut cur = self.head.get();

        while !cur.is_null() {
            unsafe {
                len += (*cur).len;
                cur = (*cur).next;
            }
        }
        len
    }

    // Reserves space for at least `len` more contiguous elements, returning
    // a pointer to the space and the available capacity (which may be > `len`)
    #[inline(never)]
    pub fn reserve(&self, len: usize) -> (*mut T, usize) {
        unsafe {
            let tail = self.tail.get();
            let cap = (*tail).cap;
            if cap - (*tail).len < len {
                // Grow capacity exponentially to amortize cost of insertions
                let mut new_cap = cap.checked_mul(2).unwrap();
                while new_cap < len {
                    new_cap = new_cap.checked_mul(2).unwrap();
                }
                let new = Chunk::new(new_cap);
                
                (*new).prev = tail;
                (*tail).next = new;
                self.tail.set(new);
            }

            let tail = self.tail.get();
            let ptr = (*tail).items.as_mut_ptr().offset((*tail).len as isize);
            let cap = (*tail).cap - (*tail).len;
            (ptr, cap)
        }
    }

    // Adds to length of curent chunk.  Usually used after
    // writing into reserved space.
    pub unsafe fn add_len(&self, len: usize) {
        let tail = self.tail.get();
        (*tail).len = len.checked_add((*tail).len).unwrap();
    }

    #[inline]
    pub fn push(&self, elem: T) -> &T {
        let (ptr, _) = self.reserve(1);
        unsafe {
            ptr::write(ptr, elem);
            self.add_len(1);
            &*ptr
        }
    }

    pub fn push_as_slice<E: IntoIterator<Item=T>>(&self, elems: E) -> &[T]
            where E::IntoIter: ExactSizeIterator {
        let iter = elems.into_iter();
        let len = iter.len();
        let (ptr, _) = self.reserve(len);
        let mut cur = ptr;
        unsafe {
            for elem in iter {
                ptr::write(cur, elem);
                cur = cur.offset(1);
            }
            self.add_len(len);
            slice::from_raw_parts(ptr, len)
        }
    }

    pub fn chunks(&self) -> Chunks<T> {
        Chunks {
            start: self.head.get(),
            end: self.tail.get(),
            _ph: PhantomData
        }
    }

    pub fn items(&self) -> Items<T> {
        #[inline(always)]
        fn id<T>(x: T) -> T { x }
        Items(self.chunks().flat_map(id))
    }
}

impl<'a, T: 'a> IntoIterator for &'a MonoVec<T> {
    type Item = &'a T;
    type IntoIter = Items<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items()
    }
}

impl MonoVec<u8> {
    pub fn format(&self, args: fmt::Arguments) -> &str {
        let mut needed = 1;
        loop {
            let (ptr, len) = self.reserve(needed);
            let mut slice = unsafe { slice::from_raw_parts_mut(ptr, len) };
            let res = slice.write_fmt(args);
            match res {
                Ok(()) => unsafe {
                    let len = len - slice.len();
                    self.add_len(len);
                    return str::from_utf8_unchecked(slice::from_raw_parts(ptr, len))
                },
                Err(ref err) if err.kind() == io::ErrorKind::WriteZero => {
                    needed = len + 1;
                    continue
                }
                Err(err) => panic!(err)
            }
        }
    }
}

impl io::Write for MonoVec<u8> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let (ptr, len) = self.reserve(0);
        let len = cmp::min(len, buf.len());
        if len != 0 {
            unsafe {
                ptr::copy_nonoverlapping(buf.as_ptr(), ptr, len);
            }
        }
        Ok(len)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<T> Drop for MonoVec<T> {
    fn drop(&mut self) {
        unsafe {
            let mut chunk = self.head.get();
            let mut next;
            while !chunk.is_null() {
                next = (*chunk).next;
                if intrinsics::needs_drop::<T>() {
                    let mut cur = (*chunk).items.as_mut_ptr();
                    let end = cur.offset((*chunk).len as isize);
                    while cur < end {
                        intrinsics::drop_in_place(cur);
                        cur = cur.offset(1);
                    }
                }
                heap::deallocate(chunk as *mut u8,
                                 (*chunk).cap * mem::size_of::<T>(),
                                 mem::min_align_of::<T>());
                chunk = next;
            }
        }
    }
}

pub struct Chunks<'a, T: 'a> {
    start: *mut Chunk<T>,
    end: *mut Chunk<T>,
    _ph: PhantomData<&'a [T]>
}

impl<'a, T> Iterator for Chunks<'a, T> {
    type Item = &'a [T];

    fn next(&mut self) -> Option<&'a [T]> {
        let chunk = self.start;
        if chunk.is_null() {
            None
        } else {
            unsafe {
                if chunk == self.end {
                    self.start = ptr::null_mut();
                    self.end = ptr::null_mut()
                } else {
                    self.start = (*chunk).next
                }
                Some(slice::from_raw_parts((*chunk).items.as_ptr(), (*chunk).len))
            }
        }
    }
}

unsafe impl <'a, T:Send> Send for Chunks<'a, T> {}
// FIXME: need to double check that this holds.
// It certainly doesn't for the vec itself.
unsafe impl <'a, T:Sync> Sync for Chunks<'a, T> {}

impl<'a, T> DoubleEndedIterator for Chunks<'a, T> {
    fn next_back(&mut self) -> Option<&'a [T]> {
        let chunk = self.end;
        if chunk.is_null() {
            None
        } else {
            unsafe {
                if chunk == self.start {
                    self.start = ptr::null_mut();
                    self.end = ptr::null_mut()
                } else {
                    self.end = (*chunk).prev
                }
                Some(slice::from_raw_parts((*chunk).items.as_ptr(), (*chunk).len))
            }
        }
    }
}

// Wrapper to hide ugly adapter type
pub struct Items<'a, T: 'a>(iter::FlatMap<Chunks<'a, T>, &'a [T], fn(&'a [T]) -> &'a [T]>);

impl<'a, T: 'a> Iterator for Items<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<&'a T> {
        self.0.next()
    }
}

impl<'a, T: 'a> DoubleEndedIterator for Items<'a, T> {
    fn next_back(&mut self) -> Option<&'a T> {
        self.0.next_back()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn format() {
        let buffer = MonoVec::new();
        for i in 0..100 {
            assert_eq!(buffer.format(format_args!("hello {}", i)),
                       format!("hello {}", i));
        }
    }
}
