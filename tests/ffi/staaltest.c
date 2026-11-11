/* The C library the ffi tests call (tests/ffi_test.rs builds it with cc). Each
   function exercises one rule of the boundary: widths and signs of integers, bool,
   text in and out, six arguments, an effect visible outside the process, and a
   crash. */
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int64_t add64(int64_t a, int64_t b) { return a + b; }
int32_t neg32(int32_t x) { return -x; }
/* The callee sees only the low bits of a narrow parameter. */
int32_t take_i8(int8_t x) { return x; }
int32_t take_u16(uint16_t x) { return x; }
/* A narrow result: only its bits are read. */
uint8_t low_byte(uint32_t x) { return (uint8_t)x; }
int8_t as_i8(int32_t x) { return (int8_t)x; }
uint64_t too_big(void) { return UINT64_MAX; }
bool is_even(int64_t x) { return x % 2 == 0; }
uint64_t text_len(const char *s) { return strlen(s); }
const char *greeting(void) { return "hello from C"; }
const char *no_text(void) { return NULL; }
const char *not_utf8(void) { return "\xff\xfe"; }
int64_t sum6(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, int64_t f) {
    return a + 10 * b + 100 * c + 1000 * d + 10000 * e + 100000 * f;
}
/* Appends a line to a file: an effect outside the process, counted by the tests. */
int64_t append_line(const char *path, const char *line) {
    FILE *f = fopen(path, "a");
    if (!f) return -1;
    fputs(line, f);
    fputs("\n", f);
    fclose(f);
    return 1;
}
/* A counter that is wrong if two calls overlap: it reads, waits, then writes. */
static int64_t counter = 0;
int64_t next(void) {
    int64_t seen = counter;
    usleep(2000);
    counter = seen + 1;
    return counter;
}
int64_t crash(void) { abort(); }

/* Hostile functions: each tries to reach outside its process, which a contained
   function must not be able to do (tests/ffi_test.rs, "contained_c_cannot_..."). */
#include <fcntl.h>
#include <sys/socket.h>
int64_t open_socket(void) { return socket(AF_INET, SOCK_STREAM, 0); }
int64_t run_program(const char *cmd) { return system(cmd); }
/* A variable of the run's environment, or no text when there is none. */
const char *read_env(const char *name) { return getenv(name); }
int64_t file_size(const char *path) {
    FILE *f = fopen(path, "r");
    if (!f) return -1;
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fclose(f);
    return n;
}
/* Writes to a descriptor number: one inherited from the process, or the worker's own
   channel to its parent. */
int64_t write_fd(int32_t fd, const char *text) { return write(fd, text, strlen(text)); }
