# While benchmark

```text
timestamp_utc=2026-09-27T03:21:26Z
revision=fae2f81ad8d2079ac9117d4d1484ec37cf6ecd21
working_tree_dirty=true
os=Darwin 25.6.0
arch=aarch64
cpu=Apple M1 Pro
rust=rustc 1.97.1 (8bab26f4f 2026-07-14)
cc=Apple clang version 21.0.0 (clang-2100.1.1.101)
debug_assertions=false
iterations=[0, 1000, 100000]
samples=30
sample_target_ms=50
seed=7
codegen=current backend defaults, no benchmark-specific overrides
cranelift_default_opt_level=none
call_boundary=AOT native symbol; JIT and Interpreter checked VmEngine API
```

Steady-state execution; excludes compilation/linking/loading. Includes the call boundary.
N=0 measures call/dispatch overhead; ns/iteration is median/N without subtraction.
Batches adapt per backend, after calibration and one untimed warm-up batch.
Backend order rotates each sample. P95 uses the nearest-rank definition.
Ratios describe this machine/run, not a language-wide performance guarantee.

| Kernel | N | Backend | Calls/sample | Median ns/call | P95 ns/call | ns/iteration | vs AOT |
|---|---:|---|---:|---:|---:|---:|---:|
| while_sum | 0 | AOT | 10000000 | 2.292 | 2.365 |  | 1.000x |
| while_sum | 0 | JIT | 1993936 | 25.484 | 26.607 |  | 11.119x |
| while_sum | 0 | Interpreter | 198686 | 252.619 | 264.951 |  | 110.220x |
| while_sum | 1000 | AOT | 77764 | 644.289 | 647.529 | 0.644289 | 1.000x |
| while_sum | 1000 | JIT | 14431 | 658.965 | 673.859 | 0.658965 | 1.023x |
| while_sum | 1000 | Interpreter | 435 | 115148.994 | 117045.593 | 115.148994 | 178.723x |
| while_sum | 100000 | AOT | 792 | 63287.537 | 65693.340 | 0.632875 | 1.000x |
| while_sum | 100000 | JIT | 793 | 63450.872 | 65633.197 | 0.634509 | 1.003x |
| while_sum | 100000 | Interpreter | 5 | 11410704.200 | 11643883.400 | 114.107042 | 180.299x |
| while_recurrence | 0 | AOT | 10000000 | 2.267 | 2.297 |  | 1.000x |
| while_recurrence | 0 | JIT | 2130610 | 23.552 | 23.752 |  | 10.389x |
| while_recurrence | 0 | Interpreter | 194632 | 256.011 | 258.612 |  | 112.927x |
| while_recurrence | 1000 | AOT | 40520 | 1244.214 | 1267.145 | 1.244214 | 1.000x |
| while_recurrence | 1000 | JIT | 39699 | 1269.016 | 1317.257 | 1.269016 | 1.020x |
| while_recurrence | 1000 | Interpreter | 342 | 147060.490 | 189311.038 | 147.060490 | 118.196x |
| while_recurrence | 100000 | AOT | 242 | 127107.866 | 136097.624 | 1.271079 | 1.000x |
| while_recurrence | 100000 | JIT | 400 | 127062.135 | 129398.023 | 1.270621 | 1.000x |
| while_recurrence | 100000 | Interpreter | 4 | 14663093.750 | 15002427.250 | 146.630937 | 115.359x |
| while_sequence | 0 | AOT | 10000000 | 2.349 | 2.413 |  | 1.000x |
| while_sequence | 0 | JIT | 2348330 | 21.670 | 22.488 |  | 9.225x |
| while_sequence | 0 | Interpreter | 176842 | 280.504 | 283.603 |  | 119.412x |
| while_sequence | 1000 | AOT | 77649 | 644.321 | 648.218 | 0.644321 | 1.000x |
| while_sequence | 1000 | JIT | 76295 | 660.156 | 672.751 | 0.660156 | 1.025x |
| while_sequence | 1000 | Interpreter | 393 | 127503.922 | 130842.662 | 127.503922 | 197.889x |
| while_sequence | 100000 | AOT | 795 | 63509.487 | 64714.623 | 0.635095 | 1.000x |
| while_sequence | 100000 | JIT | 778 | 63458.093 | 65479.221 | 0.634581 | 0.999x |
| while_sequence | 100000 | Interpreter | 4 | 12669244.875 | 12830041.500 | 126.692449 | 199.486x |
