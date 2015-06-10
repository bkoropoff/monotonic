# Monotonic #

This is a work-in-progress library of Rust data structures which
grow monotonically.  This permits appending or inserting into them
while holding references into their interior.  Compared to mechanisms
like `RefCell`, this does not incur a runtime check or potentially
panic.

This currently uses Rust features which require nightly builds.

The name of the library is subject to change since it's a bit
of a misnomer.  It's possible to support removing or mutating
elements through `&mut self` methods, but this obviously precludes
some of the intended use cases.

## Work so far ##

### `Chain<T>` ###

Like a `Vec<T>`, except memory chunks are kept in a linked list as
the structure grows rather than being reallocated.  This permits
appending through `&self`.

### `DynChain<E>` ###

Similar to `Chain`, but allows appending arbitrary types which
can erase to `E`, e.g. by unsizing:

```rust
let vec: DynChain<Display> = DynChain::new();
vec.push(42);
vec.push(3.14);
vec.push("Lasagna");

for item in &vec {
    println!("{}", item);
}
```

Elements are stored contiguously in the chunks of the underlying chain,
interspered with metadata words and any alignment padding.  If the
stored types have the same minimum alignment as `usize`, the overhead
is one `usize` per element.

### `Zone<T>` ###

A thin wrapper around `Chain<T>` which acts as a zone allocator
of `T`s.  The contents cannot be iterated, but in return freshly
allocated elements are mutable.

When `T` is `Copy`, you can use `Zone::alloc` to allocate space
for a contiguous chunk of elements, then fill the space incrementally
through the returned `Quota` handle.  Once filled, `Quota::into_slice`
converts the handle into a mutable slice of the allocated elements.
Note that if multiple allocations are made simultaneously, unused space
between them can be wasted.

For the special case of `Zone<u8>`, returned quota handles implement
the `std::io::Write` trait.  You can also use `Zone::alloc_str` to
acquire a `QuotaStr` handle, which implements `std::fmt::Write`.
The `Zone::format` will handle allocating enough space to fit the
entire output of a format operation and return the resulting string
slice.

### `DynZone` ###

A thin wrapper around `DynChain` which permits allocating different
types in the same zone at the cost of metadata and padding overhead.
