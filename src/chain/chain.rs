use std::fmt;
use std::mem;
use std::ptr;
use std::slice;
use std::iter;
use std::io::{self, Write};
use std::cmp;
use std::rt::heap;
use std::cell::Cell;
use std::marker::PhantomData;
use std::intrinsics;

// A chunk in the chain
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

pub struct Chain<T> {
    head: Cell<*mut Chunk<T>>,
    tail: Cell<*mut Chunk<T>>,
    _ph: PhantomData<T>
}

unsafe impl<T: Send> Send for Chain<T> {}

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
            if res.is_null() {
                panic!("Chain: failed to allocate chunk!")
            }
            ptr::write(&mut (*res).prev, ptr::null_mut());
            ptr::write(&mut (*res).next, ptr::null_mut());
            ptr::write(&mut (*res).len, 0);
            ptr::write(&mut (*res).cap, cap);
            res
        }
    }
}

impl<T> Chain<T> {
    pub fn new() -> Self {
        Self::with_capacity(8)
    }

    pub fn with_capacity(cap: usize) -> Self {
        let head = Chunk::new(cmp::max(cap, 1));
        Chain {
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
        (*tail).len += len;
    }

    // Shrinks length of allocation at (ptr, ptr + old_len) if possible
    pub unsafe fn shrink_len(&self, ptr: *mut T, old_len: usize, new_len: usize) {
        let tail = self.tail.get();
        if ptr.offset(old_len as isize) == (*tail).items.as_mut_ptr().offset((*tail).len as isize) {
            (*tail).len = (*tail).len - old_len + new_len;
        }
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

    pub fn extend_as_slice<E: IntoIterator<Item=T>>(&self, elems: E) -> &[T]
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

    pub fn clear(&mut self) {
        unsafe {
            loop {
                let chunk = self.head.get();
                self.head.set((*chunk).next);
                if intrinsics::needs_drop::<T>() {
                    let mut ptr = (*chunk).items.as_mut_ptr();
                    let end = ptr.offset((*chunk).len as isize);
                    while ptr < end {
                        intrinsics::drop_in_place(ptr);
                        ptr = ptr.offset(mem::size_of::<T>() as isize);
                    }
                }
                if chunk == self.tail.get() {
                    break
                }
                heap::deallocate(chunk as *mut u8,
                                 mem::size_of::<Chunk<T>>() + (*chunk).len * mem::size_of::<T>(),
                                 mem::align_of::<Chunk<T>>());
            }
            let save = self.tail.get();
            self.head.set(save);
            (*save).len = 0;
        }
    }

    pub fn chunks(&self) -> Chunks<T> {
        Chunks {
            start: self.head.get(),
            end: self.tail.get(),
            _ph: PhantomData
        }
    }

    pub fn chunks_mut(&mut self) -> ChunksMut<T> {
        ChunksMut {
            start: self.head.get(),
            end: self.tail.get(),
            _ph: PhantomData
        }
    }

    pub fn iter(&self) -> Iter<T> {
        #[inline(always)]
        fn id<T>(x: T) -> T { x }
        Iter(self.chunks().flat_map(id))
    }

    pub fn iter_mut(&mut self) -> IterMut<T> {
        #[inline(always)]
        fn id<T>(x: T) -> T { x }
        IterMut(self.chunks_mut().flat_map(id))
    }
}

impl<'a, T: 'a> IntoIterator for &'a Chain<T> {
    type Item = &'a T;
    type IntoIter = Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a, T: 'a> IntoIterator for &'a mut Chain<T> {
    type Item = &'a mut T;
    type IntoIter = IterMut<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<T> IntoIterator for Chain<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        unsafe {
            let start = self.head.get();
            let end = self.tail.get();
            mem::forget(self);
            IntoIter {
                start: start,
                end: end,
                front: (*start).items.as_mut_ptr(),
                _ph: PhantomData
            }
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Chain<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut need_comma = false;
        try!(write!(f, "["));
        for elem in self {
            if need_comma {
                try!(write!(f, ", "));
            }
            try!(elem.fmt(f));
            need_comma = true;
        }
        write!(f, "]")
    }
}

impl io::Write for Chain<u8> {
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

impl<T> Drop for Chain<T> {
    fn drop(&mut self) {
        self.clear();
        let chunk = self.head.get();
        unsafe {
            heap::deallocate(chunk as *mut u8,
                             mem::size_of::<Chunk<T>>() + (*chunk).len * mem::size_of::<T>(),
                             mem::align_of::<Chunk<T>>());

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

pub struct ChunksMut<'a, T: 'a> {
    start: *mut Chunk<T>,
    end: *mut Chunk<T>,
    _ph: PhantomData<&'a mut [T]>
}

impl<'a, T> Iterator for ChunksMut<'a, T> {
    type Item = &'a mut [T];

    fn next(&mut self) -> Option<&'a mut [T]> {
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
                Some(slice::from_raw_parts_mut((*chunk).items.as_mut_ptr(), (*chunk).len))
            }
        }
    }
}

unsafe impl <'a, T:Send> Send for ChunksMut<'a, T> {}
unsafe impl <'a, T:Sync> Sync for ChunksMut<'a, T> {}

impl<'a, T> DoubleEndedIterator for ChunksMut<'a, T> {
    fn next_back(&mut self) -> Option<&'a mut [T]> {
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
                Some(slice::from_raw_parts_mut((*chunk).items.as_mut_ptr(), (*chunk).len))
            }
        }
    }
}

// Wrapper to hide ugly adapter type
pub struct Iter<'a, T: 'a>(iter::FlatMap<Chunks<'a, T>, &'a [T], fn(&'a [T]) -> &'a [T]>);

impl<'a, T: 'a> Iterator for Iter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<&'a T> {
        self.0.next()
    }
}

impl<'a, T: 'a> DoubleEndedIterator for Iter<'a, T> {
    fn next_back(&mut self) -> Option<&'a T> {
        self.0.next_back()
    }
}

// Wrapper to hide ugly adapter type
pub struct IterMut<'a, T: 'a>(iter::FlatMap<ChunksMut<'a, T>, &'a mut [T],
                              fn(&'a mut [T]) -> &'a mut [T]>);

impl<'a, T: 'a> Iterator for IterMut<'a, T> {
    type Item = &'a mut T;
    fn next(&mut self) -> Option<&'a mut T> {
        self.0.next()
    }
}

impl<'a, T: 'a> DoubleEndedIterator for IterMut<'a, T> {
    fn next_back(&mut self) -> Option<&'a mut T> {
        self.0.next_back()
    }
}

pub struct IntoIter<T> {
    start: *mut Chunk<T>,
    end: *mut Chunk<T>,
    front: *mut T,
    _ph: PhantomData<T>
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        loop {
            unsafe {
                let chunk = self.start;
                let back = (*chunk).items.as_mut_ptr().offset((*chunk).len as isize);
                if self.front == back {
                    if self.start == self.end {
                        return None
                    }
                    self.start = (*chunk).next;
                    heap::deallocate(chunk as *mut u8,
                                     mem::size_of::<Chunk<T>>() + (*chunk).cap * mem::size_of::<T>(),
                                     mem::min_align_of::<Chunk<T>>());
                    self.front = (*self.start).items.as_mut_ptr();
                    continue;
                }
                let ptr = self.front;
                self.front = self.front.offset(1);

                return Some(ptr::read(ptr))
            }
        }
    }
}

unsafe impl <T: Send> Send for IntoIter<T> {}
unsafe impl <T: Sync> Sync for IntoIter<T> {}

impl<T> DoubleEndedIterator for IntoIter<T> {
    fn next_back(&mut self) -> Option<T> {
        loop {
            unsafe {
                let chunk = self.end;
                let back = (*chunk).items.as_mut_ptr().offset((*chunk).len as isize);
                if back == self.front {
                    if chunk == self.start {
                        return None
                    }
                    self.end = (*chunk).prev;
                    heap::deallocate(
                        chunk as *mut u8,
                        mem::size_of::<Chunk<T>>() + (*chunk).cap * mem::size_of::<T>(),
                        mem::min_align_of::<Chunk<T>>());
                    continue;
                }
                (*chunk).len -= 1;
                let ptr = (*chunk).items.as_mut_ptr().offset((*chunk).len as isize);
                return Some(ptr::read(ptr))
            }
        }
    }
}

impl<T> Drop for IntoIter<T> {
    fn drop(&mut self) {
        while let Some(_) = self.next() {}
        debug_assert!(self.start == self.end);
        unsafe {
            heap::deallocate(self.start as *mut u8,
                             mem::size_of::<Chunk<T>>() + (*self.start).cap * mem::size_of::<T>(),
                             mem::min_align_of::<Chunk<T>>());
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn drop_type() {
        static mut COUNT : usize = 0;

        struct DropType;

        impl DropType {
            fn new() -> DropType {
                unsafe { COUNT += 1; }
                DropType
            }
        }

        impl Drop for DropType {
            fn drop(&mut self) {
                unsafe { COUNT -= 1 }
            }
        }

        {
            let chain = Chain::new();

            chain.push(DropType::new());
            chain.push(DropType::new());
            chain.push(DropType::new());

            assert_eq!(unsafe { COUNT }, 3);
        }

        assert_eq!(unsafe { COUNT }, 0);
    }
}
