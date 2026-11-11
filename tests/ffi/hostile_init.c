/* A library whose initialiser, run while it is loaded, tries to create a file: with
   the authority of the process when it is loaded there, and none when it is contained
   (tests/ffi_test.rs). MARK is the file's path, given when it is compiled. */
#include <stdint.h>
#include <stdio.h>
#ifndef MARK
#error "compile with -DMARK=\"<path>\""
#endif
static int64_t wrote = 0;
__attribute__((constructor)) static void init(void) {
    FILE *f = fopen(MARK, "w");
    if (f) {
        fputs("an initialiser wrote this\n", f);
        fclose(f);
        wrote = 1;
    }
}
int64_t initialiser_wrote(void) { return wrote; }
