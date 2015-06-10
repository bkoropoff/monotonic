use super::chain::{Chain, DynChain, Erase};
use std::mem;
use std::ptr;
use std::cmp;
use std::fmt;
use std::io;
use std::slice;
use std::intrinsics;

pub struct Zone<T> {
    chain: Chain<T>
}

impl<T> Zone<T> {
    #[inline]
    pub fn new() -> Self {
        Self::with_capacity(8)
    }

    #[inline]
    pub fn with_capacity(count: usize) -> Self {
        Zone {
            chain: Chain::with_capacity(count)
        }
    }

    #[inline]
    #[allow(mutable_transmutes)]
    pub fn push(&self, elem: T) -> &mut T {
        unsafe { mem::transmute(self.chain.push(elem)) }
    }

    // We only permit allocation of chunks for Copy types
    // since the caller can fail to fill the entire chunk,
    // leaving uninitialized values that would be hit on
    // drop.
    pub fn alloc(&self, len: usize) -> Quota<T> where T: Copy {
        unsafe {
            let (origin, cap) = self.chain.reserve(len);
            self.chain.add_len(cap);
            Quota {
                origin: origin,
                len: 0,
                cap: cap,
                arena: self
            }
        }
    }
}

impl Zone<u8> {
    pub fn alloc_str(&self, len: usize) -> StrQuota {
        StrQuota(self.alloc(len))
    }
    
    pub fn format(&self, args: fmt::Arguments) -> &str {
        let mut len = 32;
        loop {
            let mut quota = self.alloc_str(len);
            if let Ok(()) = fmt::write(&mut quota, args) {
                return quota.into_slice()
            }
            quota.clear();
            // Due to the way the underlying MonoVec allocates,
            // incrementing by one is sufficient to force it to
            // grow by 2x, so this is not as silly as it looks
            len = quota.capacity() + 1;
        }
    }
}

// A Quota is basically a write-only Vec pointing into a Zone
// that can be converted into a slice after filling it
pub struct Quota<'a, T: 'a> {
    origin: *mut T,
    len: usize,
    cap: usize,
    arena: &'a Zone<T>
}

impl<'a, T> Quota<'a, T> {
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.cap
    }
    
    #[inline]
    pub fn push(&mut self, elem: T) -> Result<(), T> {
        if self.cap == self.len {
            Err(elem)
        } else {
            unsafe {
                ptr::write(self.origin.offset(self.len as isize), elem);
                self.len += 1
            }
            Ok(())
        }
    }

    #[inline]    
    pub fn fill(&mut self, data: &[T]) -> usize where T: Copy {
        unsafe {
            let len = cmp::min(self.cap - self.len, data.len());
            ptr::copy_nonoverlapping(data.as_ptr(), self.origin.offset(self.len as isize), len);
            self.len += len;
            len
        }
    }
    
    pub fn extend<E>(&mut self, elems: E) -> usize 
            where E:IntoIterator<Item=T> {
        let mut count = 0;
        let mut iter = elems.into_iter();
        while self.len < self.cap {
            if let Some(elem) = iter.next() {
                unsafe {
                    ptr::write(self.origin.offset(self.len as isize), elem);
                    self.len += 1;
                    count += 1;
                }
            } else {
                break
            }
        }
        
        count
    }
    
    pub fn clear(&mut self) {
        unsafe {
            if intrinsics::needs_drop::<T>() {
                let mut ptr = self.origin;
                let end = self.origin.offset(self.len as isize);
                while ptr < end {
                    intrinsics::drop_in_place(ptr);
                    ptr = ptr.offset(1);
                }
                self.len = 0;
            }
        }
    }
    
    #[inline]
    pub fn into_slice(self) -> &'a mut [T] {
        unsafe {
            slice::from_raw_parts_mut(self.origin, self.len)
        }
    }
}

impl<'a, T> Drop for Quota<'a, T> {
    fn drop(&mut self) {
        // Shrink the allocation if we haven't already allocated more space past it.
        unsafe {
            self.arena.chain.shrink_len(self.origin, self.cap, self.len)
        }
    }
}

impl<'a> io::Write for Quota<'a, u8> {
    #[inline]
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        Ok(self.fill(data))
    }
    #[inline]
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

pub struct StrQuota<'a>(Quota<'a, u8>);

impl<'a> StrQuota<'a> {
    #[inline]
    pub fn len(&self) -> usize {
        self.0.len
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }
    
    #[inline]
    pub fn fill(&mut self, data: &str) -> usize {
        self.0.fill(data.as_bytes())
    }
    
    pub fn clear(&mut self) {
        self.0.clear()
    }
    
    #[inline]
    pub fn into_slice(self) -> &'a str {
        unsafe { mem::transmute(self.0.into_slice()) }
    }
}

impl<'a> fmt::Write for StrQuota<'a> {
    #[inline]
    fn write_str(&mut self, data: &str) -> fmt::Result {
        let len = self.fill(data);
        if len < data.len() {
            Err(fmt::Error)
        } else {
            Ok(())
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

pub struct DynZone<'gt> {
    chain: DynChain<'gt, (), Forget>
}

impl<'gt> DynZone<'gt> {
    #[allow(mutable_transmutes)]
    pub fn alloc<T: 'gt, F: FnOnce() -> T>(&self, f: F) -> &mut T {
        // FIXME: we need a way to emplace inside the underlying chain
        unsafe { mem::transmute(self.chain.push(f())) }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn format() {
        let buffer = Zone::new();
        for i in 0..100 {
            assert_eq!(buffer.format(format_args!("hello {}", i)),
                       format!("hello {}", i));
        }
    }
}
