# Mutable Map Hash Index

MutMap and MutSet share a dense entry array and a separate bucket array. Each
entry owns its flattened key/value words and ownership masks, caches its hash,
and records the previous/next entry indices in its collision chain. The bucket
count equals entry capacity, starts at four and doubles when full. Both arrays
are allocated before replacing the previous buffers.

Lookup selects a bucket from the hash, filters candidates by the full cached
hash, then calls the key's equality method. Replacement preserves the chain
links and returns the previous value. Removal unlinks the entry, moves the last
entry into the vacated slot and repairs its links. It does not shift the array
suffix or shrink capacity. There is no entry-order guarantee in the public API.

Growth rebuilds chains from cached hashes without calling user Hash or equality
methods. Hashing occurs once per key operation. With well-distributed hashes,
lookup/removal/replacement are expected O(1), and insertion is amortized O(1).
Worst-case collisions still cost O(n); resizing costs O(n). These costs exclude
the user's Hash/equality bodies and key/value cloning or destruction.

Generated code always supplies the existing hash/equality descriptor. Legacy
0/1 runtime descriptors retain the compatibility scan, including lookups that
mix legacy and generated descriptors. No runtime function signatures, compiler
metadata or public APIs change; the opaque payload layout is runtime-private.
Immutable Map and Set use a separate [persistent HAMT](persistent-map.md).

## Verification

Runtime tests compare deterministic insertion/deletion churn against BTreeMap
under distinct hashes, identical hashes and distinct hashes sharing a bucket.
They check every chain and dense-array invariant, hash-call counts across growth,
capacity reuse, replacement at full capacity and shared String cleanup. The
existing CLI Hash package also exercises growth and reinsertion for imported
keys and colliding MutMap/MutSet keys through JIT, cached JIT and both AOT modes.

## Storage Benchmark

```sh
cargo test --manifest-path crates/joky-runtime/Cargo.toml --release \
  bench_mut_map_index -- --ignored --nocapture
```

This ignored benchmark measures runtime storage directly with one-word keys and
values, a simple native hash callback, permuted accesses, one warmup and five
measured samples. Each phase reports its median nanoseconds per operation.
Insertion includes growth, deletion starts from a full map, and replacement
updates every key. Lookup/miss phases each perform 20,000 operations. Map setup
and final destruction are outside the timed phases. Assertions remain enabled.
It excludes JIT compilation, Joky Hasher allocation and user trait method costs.

Local aarch64 macOS results, comparing the linear implementation at `306458e`
with the index using the same benchmark (2026-09-19):

| Keys | Operation | Linear ns/op | Indexed ns/op |
| ---: | --- | ---: | ---: |
| 128 | Insert | 221 | 131 |
| 128 | Get | 131 | 71 |
| 128 | Miss | 133 | 30 |
| 128 | Replace | 273 | 196 |
| 128 | Remove | 146 | 125 |
| 1024 | Insert | 661 | 168 |
| 1024 | Get | 546 | 94 |
| 1024 | Miss | 777 | 38 |
| 1024 | Replace | 804 | 241 |
| 1024 | Remove | 411 | 158 |
| 8192 | Insert | 5117 | 114 |
| 8192 | Get | 3962 | 69 |
| 8192 | Miss | 5951 | 28 |
| 8192 | Replace | 5204 | 193 |
| 8192 | Remove | 4018 | 114 |

Timings are observations, not regression thresholds. The benchmark's hash
distribution is favorable to the index; collision correctness is checked by
tests rather than inferred from these timings. The index adds one bucket word
and two chain-index words per capacity slot (24 bytes on this 64-bit target),
in addition to the existing entry and payload storage.
