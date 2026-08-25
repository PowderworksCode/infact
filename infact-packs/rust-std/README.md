# rust-std

Callable signatures for the Rust standard library, built from the rustdoc JSON
that ships as a rustup component rather than from a checkout of the compiler:

```sh
rustup component add rust-docs-json --toolchain nightly
J=$(rustc +nightly --print sysroot)/share/doc/rust/json
V=$(rustc +nightly --version | awk '{print $2}')
infact catalog "$J/core.json" --package core --version "$V" \
    --output "infact-packs/rust-std/api/core-$V.json"
```

`alloc.json` and `std.json` build the same way and are not kept here, because
nothing yet matches against anything they add. Regenerating one takes two
seconds; carrying it does not.

Only a nightly toolchain emits rustdoc JSON, so the version this records is a
nightly version. That is what the finding is bound to, and it is honest: the
signature a recommendation was checked against came from that compiler and no
other.

`core.json` is large — 26 MB against itertools' 312 KB — because a language's
standard library is large and because rustdoc emits every item, including ones
no caller can name. A catalog that dropped them would be smaller and would be
asserting a visibility rule nothing here has checked.
