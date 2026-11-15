# Calling C functions

A program calls a function of a C library through a capability the operator grants.
Each function is its own capability, `ffi.<library>.<function>`. By default the
library runs in a worker process that the kernel allows only to compute. The full
rules are in [RUNTIME.md](../RUNTIME.md), section 19.

## Declaring

```staal file=checksum.staal
foreign ffi.zlib {
    fn crc32(crc: u64, data: text, length: u32) -> u64
}

fn checksum(data: Text, crc: cap ffi.zlib.crc32(uses: 1)) -> Int {
    return crc.crc32(0, data, len(data))
}
```

- A library name is lowercase letters, digits and `_`. A library is declared once per
  program, each function once, with at most six parameters.
- A `cap ffi.*` must name a declared function (E101).
- Only `uses` applies. `max`, `each` and `only` are refused.
- A function's return value is untrusted (E006). An `Outcome` of `try` around a call
  holds no receipt (E008).
- A package may declare only `foreign ffi.<its name>`, and must list each function it
  uses as `"ffi.<name>.<function>": "any"` in its `authority` (E108).

| C type | Staal type | Passed | Returned |
| --- | --- | --- | --- |
| `i8`, `i16`, `i32`, `i64` | `Int` | Must fit the type, or the call is refused. | Sign-extended. |
| `u8`, `u16`, `u32` | `Int` | Must fit the type. | Zero-extended. |
| `u64` | `Int` | 0 to 2^63-1. | Must be at most 2^63-1, or the call fails. |
| `bool` | `Bool` | 0 or 1. | Any nonzero low byte is true. |
| `text` | `Text` | A NUL-terminated copy, valid during the call. Text containing NUL is refused. | A NUL-terminated string, copied (at most 1 MiB, UTF-8) and never freed. |
| `void` | `Unit` | Not a parameter. | The default without `->`. |

There are no floating-point numbers, pointers, buffers, structs, callbacks or variadic
functions. Grants are accepted on x86_64 and aarch64, Linux and macOS. Nothing checks
the declaration against the library.

## Granting

```json file=checksum.json
{
  "entry": "checksum",
  "inputs": { "data": "hello" },
  "grants": {
    "crc": {
      "cap": "ffi.zlib.crc32",
      "library": "/usr/lib/x86_64-linux-gnu/libz.so.1.3",
      "sha256": "f61677c7bdb7c2aa2a243ea781bfd16afcb361cede7b4ef76127a9d8eb9a5242"
    }
  }
}
```

| Key | Contents |
| --- | --- |
| `library` | Required. The library file's absolute path. |
| `sha256` | Required. The file's SHA-256, 64 lowercase hexadecimal digits, as `sha256sum` (Linux) or `shasum -a 256` (macOS) prints it. |
| `isolation` | `"contained"` (default), `"separate"` or `"none"`. |
| `repeatable` | `true` if a call cut short by a crash may be made again: the function changes nothing, or changing it twice is harmless. |

When the run starts, before anything else, the file is opened, refused if it is not a
regular file or if any user can write to it, and hashed. A hash that does not match is
a config error (exit code 2):

```text
staalc: grants.crc: /usr/lib/x86_64-linux-gnu/libz.so.1.3 has SHA-256 f61677c7bdb7c2aa2a243ea781bfd16afcb361cede7b4ef76127a9d8eb9a5242, not the 0000000000000000000000000000000000000000000000000000000000000000 the grant names
```

On Linux the library is loaded through the descriptor that was hashed. On macOS it is
loaded by path after hashing. Only that file is pinned: libraries it depends on are
found by the system loader. Loading runs the library's initialisers.

The run prints `907060870`, the CRC-32 of `hello`.

## Isolation

| `isolation` | Where the C code runs | What it can do | The run |
| --- | --- | --- | --- |
| `contained` | A worker process, restricted by seccomp | Compute only | Checked |
| `separate` | A worker process | Anything that process can | Unchecked |
| `none` | The run's process | Anything the run's process can | Unchecked |

`contained` requires Linux on x86_64 or aarch64; elsewhere, a grant without
`isolation` is a config error. On macOS, use `separate` or `none`. One library is run
one way in a run.

A contained worker:

- is started fresh for each library and ends with the run;
- has an empty environment except `LD_LIBRARY_PATH` and `DYLD_LIBRARY_PATH`, so no
  secret in the run's environment reaches C code;
- runs the library's initialisers under a filter that allows opening files only for
  reading, and no sockets, processes or signals to others;
- after loading, may only use its socket, memory, futexes, the clock, sleep, and exit.

A worker that crashes, makes a forbidden system call or sends a malformed reply ends;
the call fails saying which, and so does every later call to that library in the run.

The worker is `staalc` itself, an executable made by `staalc build`, or the `staalc`
installed beside the Python package's library. `STAALC_FFI_WORKER` names another (an
absolute path). Its version must match, and it and every directory above it must be
owned by root or the run's user and not writable by group or others. The Node.js
package installs no `staalc`: set `STAALC_FFI_WORKER` to call C from it.

No time limit applies to a call: a C function that never returns holds its run.

## Unchecked runs

With `separate` or `none`, the run is unchecked: the broker cannot check what the C
code does. `staalc run` and `staalc resume` print to standard error, before the result:

```text
staalc: this run is unchecked: it loaded C code for ffi.zlib.crc32, which can do anything the process can; the broker cannot check it
```

The log begins with an `unchecked` event for each such grant (`capability`, `library`,
`sha256`, `isolation`, `why`), or a `contained` event for a contained one. The C
interface's result lists the capabilities in `"unchecked"`.

## Calls and crashes

Arguments are checked against the declaration before anything is recorded. A call is
then an action: authorized, journaled with a hash of its arguments and the library,
made, and its value recorded. A resumed run replays the value without calling C. Calls
into one library are made one at a time, from any task.

| A call cut short by a crash | On resume |
| --- | --- |
| Contained library | Made again, in a new worker. |
| `"repeatable": true` | Made again. |
| Otherwise | The run stops. `staalc resolve` accepts only `--failed <reason>` or `--retry`. |
