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

### `MonoVec<T>` ###

A monotonically-growing vector of `T` which permits immutable
iteration of elements.

The `MonoVec<u8>::format` method performs string formatting and
returns a contiguous string slice of the result, making it useful
as an arena for temporary strings.

### `HetVec<E>` ###

Similar to `MonoVec`, but allows pushing arbitrary types which
can erase to `E`, e.g. by unsizing:

```rust
let vec: HetVec<Display> = HetVec::new();
vec.push(42);
vec.push(3.14);
vec.push("Lasagna");

for item in &vec {
    println!("{}", item);
}
```

Elements are stored contiguously interspered with vtable pointers and alignment
padding.

### `TypedArena<T>` ###

A thin wrapper around `MonoVec<T>` which works like the version in rustc's
libarena.

### `Arena` ###

A thin wrapper around `HetVec` which works like the version in rustc's
libarena.
