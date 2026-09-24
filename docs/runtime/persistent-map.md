# Persistent Map and Set

Map and Set use a bitmap-indexed hash array mapped trie (HAMT). The runtime
interface and key restrictions are unchanged. An empty map is still a null
pointer; a nonempty map is a shared managed root owning an `Arc<Node>`.
The opaque node layout is private to the runtime, so no ABI or metadata version
change is needed. Rebuild the runtime archive to use the new storage in AOT.

## Structure and Ownership

Each branch selects five bits of the cached 64-bit hash. A 32-bit bitmap and
population count locate children in a compact array. The final level uses the
remaining four bits, bounding branch depth at 13. Leaves hold one entry;
collision nodes hold multiple entries with identical complete hashes. Equality
callbacks distinguish entries within a collision. Every node caches its subtree
size, making `length()` constant-time.

Insert and remove copy the affected path and retain untouched subtrees. Node and
entry sharing uses standard Rust `Arc`, including across Joky worker threads.
An entry owns its flattened words and ownership masks; its final destructor
releases managed keys and values exactly once. Copying a path does not clone
managed payloads or invoke user Hash methods. Get retains the returned value
using the existing runtime ownership helpers.

Removal drops empty branches and collapses single-entry collisions. A branch
with one terminal child can collapse; a branch with one branch child must keep
its level because depth determines which hash bits it reads. Removing an absent
key returns the same root without allocating. Removing the last entry returns
null. Replacing an equal key preserves the previous API behavior of retaining
the new key representation and value in the new version.

With well-distributed hashes, lookup and updates follow a short O(log32 n) path.
An update copies at most 32 child references at each visited branch. Complete
hash collisions cost O(c) for c colliding keys and copy that collision array on
update; the worst case remains O(n). Retaining many versions keeps their unique
paths alive. These costs exclude Hash/equality bodies and value destruction.

Generated code supplies hash/equality callbacks. Legacy raw-runtime descriptors
0/1 retain structural/String comparisons using a fallback scan. Mixed legacy
and generated insertions can relocate an equal key to its new hash path; roots
with legacy entries conservatively keep scan mode. Public Joky maps use the
indexed path throughout.

## Validation

Runtime tests compare updates and saved snapshots against BTreeMap with distinct
hashes, complete collisions and long shared prefixes. They cover the highest
hash bit, branch collapse, deletion to empty, missing deletion, descriptor
mixing and shared String lifetime. Pointer checks show that replacing one of
1024 keys copies two branches and one leaf; weak references verify that every
reachable node is freed after all tested versions are released. Managed resource
counters cover roots and payloads; internal Arc nodes are not separate managed
objects. The CLI Hash parity package covers imported keys, Map/Set collisions,
old versions and parallel updates across JIT, cached JIT, debug and release AOT.

## Storage Benchmark

```sh
cargo test --manifest-path crates/joky-runtime/Cargo.toml --release \
  bench_persistent_map -- --ignored --nocapture
```

This ignored test uses scalar keys/values and a simple native hash callback,
one warmup and five measured samples. Numbers are median nanoseconds per
operation. Insert builds a map without keeping prior versions. Get/miss each
perform 20,000 permuted lookups. Persistent replace/remove create and retain 64
independent versions from the same base; their destruction is outside the timed
phase. Assertions remain enabled. JIT compilation and Joky trait/Hasher costs
are excluded. The runtime unit-test managed allocation registry is enabled;
the linear baseline registered each entry, while HAMT registers roots and uses
Arc for internal nodes. Results therefore describe this storage test, not a
production end-to-end speedup.

Local aarch64 macOS results (2026-09-19), using the same benchmark on the linear
implementation at `b80fc46` and on the HAMT:

| Keys | Operation | Linear ns/op | HAMT ns/op |
| ---: | --- | ---: | ---: |
| 128 | Insert | 14768 | 647 |
| 128 | Get | 1434 | 84 |
| 128 | Miss | 2763 | 34 |
| 128 | Persistent replace | 24231 | 389 |
| 128 | Persistent remove | 20460 | 340 |
| 1024 | Insert | 143767 | 663 |
| 1024 | Get | 12817 | 73 |
| 1024 | Miss | 25803 | 30 |
| 1024 | Persistent replace | 275273 | 420 |
| 1024 | Persistent remove | 259311 | 341 |
| 2048 | Insert | 307710 | 586 |
| 2048 | Get | 26913 | 73 |
| 2048 | Miss | 53554 | 27 |
| 2048 | Persistent replace | 611830 | 485 |
| 2048 | Persistent remove | 599529 | 374 |

These timings are observations, not regression thresholds. Hash distribution is
favorable; adversarial collision correctness is covered separately by tests.
