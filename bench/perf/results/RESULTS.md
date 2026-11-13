# Results

Measured 2026-11-13 on Intel(R) Xeon(R) Processor @ 2.10GHz, 2 cores, 8022 MiB, Linux 6.18.44-fc-v445. Median of 5 runs, in milliseconds, of the whole process; in brackets, times the Rust time. A workload's size is n.

| Workload | n | Staal interpreter | Staal native | Staal executable | Rust | Go | Python | TypeScript (Node.js) | TypeScript (Bun) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| startup |  | 2.8 (2.4×) | 2.8 (2.4×) | 2.7 (2.3×) | 1.2 (1.0×) | 1.2 (1.0×) | 13.3 (11.4×) | 37.5 (32.2×) | 4.0 (3.5×) |
| fib | 32 | 3904.8 (272.4×) | 28.6 (2.0×) | 30.1 (2.1×) | 14.3 (1.0×) | 14.6 (1.0×) | 307.9 (21.5×) | 50.8 (3.5×) | 25.0 (1.7×) |
| loop | 10000000 | 5336.1 (430.5×) | 41.9 (3.4×) | 71.0 (5.7×) | 12.4 (1.0×) | 12.9 (1.0×) | 701.9 (56.6×) | 141.2 (11.4×) | 89.7 (7.2×) |
| money | 1000000 | 608.7 (269.1×) | 211.3 (93.4×) | 223.1 (98.6×) | 2.3 (1.0×) | 2.4 (1.1×) | 220.6 (97.6×) | 29.8 (13.2×) | 9.7 (4.3×) |
| lists | 10000 | 21.9 (10.5×) | 15.1 (7.2×) | 11.6 (5.5×) | 2.1 (1.0×) | 2.8 (1.3×) | 16.9 (8.1×) | 28.1 (13.5×) | 7.6 (3.7×) |
| maps | 20000 | 68.8 (42.9×) | 52.2 (32.6×) | 49.8 (31.1×) | 1.6 (1.0×) | 1.6 (1.0×) | 16.4 (10.2×) | 28.3 (17.6×) | 7.3 (4.5×) |
| text | 50000 | 29.0 (11.1×) | 21.7 (8.3×) | 24.1 (9.2×) | 2.6 (1.0×) | 4.4 (1.7×) | 20.0 (7.7×) | 47.3 (18.1×) | 17.6 (6.7×) |

Peak memory (MiB, the most any run held; a process that does nothing shows 2.2, the launcher's floor):

| Workload | Staal interpreter | Staal native | Staal executable | Rust | Go | Python | TypeScript (Node.js) | TypeScript (Bun) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| startup | 9.7 | 12.9 | 10.5 | 2.2 | 2.2 | 8.7 | 42.2 | 11.9 |
| fib | 9.8 | 12.8 | 10.5 | 2.2 | 2.2 | 8.9 | 47.5 | 21.8 |
| loop | 9.8 | 13.1 | 10.5 | 2.2 | 2.2 | 8.9 | 49.5 | 23.0 |
| money | 9.8 | 13.4 | 10.5 | 2.2 | 2.2 | 10.1 | 47.5 | 23.0 |
| lists | 14.1 | 18.3 | 15.1 | 2.2 | 2.2 | 9.4 | 47.8 | 22.6 |
| maps | 10.2 | 14.2 | 10.9 | 2.2 | 2.2 | 9.0 | 42.5 | 18.1 |
| text | 10.1 | 13.6 | 10.5 | 2.4 | 3.6 | 12.1 | 54.0 | 24.1 |

How building a list and a text grows with n (median, milliseconds):

| Workload | n | Staal interpreter | Staal native | Staal executable | Rust | Go | Python | TypeScript (Node.js) | TypeScript (Bun) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| lists | 2500 | 8.4 | 8.4 | 5.4 | 1.3 | 1.8 | 18.9 | 35.7 | 7.9 |
| lists | 5000 | 11.1 | 9.1 | 6.8 | 1.4 | 2.0 | 17.1 | 30.3 | 6.8 |
| lists | 10000 | 21.2 | 15.3 | 9.7 | 2.1 | 2.6 | 16.7 | 28.4 | 9.2 |
| lists | 20000 | 41.3 | 24.0 | 20.4 | 2.6 | 3.9 | 26.7 | 47.3 | 15.0 |
| text | 12500 | 15.5 | 9.9 | 7.8 | 1.8 | 1.8 | 19.2 | 29.8 | 7.4 |
| text | 25000 | 15.6 | 12.6 | 11.3 | 2.0 | 2.5 | 21.1 | 31.7 | 11.9 |
| text | 50000 | 45.9 | 19.6 | 17.8 | 2.6 | 3.4 | 18.9 | 36.2 | 16.3 |
| text | 100000 | 99.4 | 43.0 | 39.3 | 5.6 | 12.6 | 38.6 | 59.4 | 32.1 |

Versions:

- staalc: staalc 0.28.0
- rustc: rustc 1.97.0 (2d8144b78 2026-07-07)
- go: go version go1.24.7 linux/amd64
- python: 3.13.16
- node: v22.22.0
- tsc: Version 6.0.3
- bun: 1.4.2
