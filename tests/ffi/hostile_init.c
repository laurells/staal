/* A library whose initialiser, run while it is loaded, tries one thing a contained
   library may not do (tests/ffi_test.rs), chosen when it is compiled:

     -DMARK="<path>"   create a file at <path>
     -DPRCTL           make the process traceable by any other (prctl, not the one
                       option the worker's own setup needs)
     -DSECCOMP         install a seccomp filter on this thread only (not the way the
                       worker installs its own)

   It runs with the authority of the process when loaded there, and initialiser_wrote
   says whether the attempt went through. */
#include <stdint.h>
#include <stdio.h>
#if defined(PRCTL) || defined(SECCOMP)
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>
#endif

static int64_t wrote = 0;

__attribute__((constructor)) static void init(void) {
#if defined(MARK)
    FILE *f = fopen(MARK, "w");
    if (f) {
        fputs("an initialiser wrote this\n", f);
        fclose(f);
        wrote = 1;
    }
#elif defined(PRCTL)
    if (prctl(PR_SET_DUMPABLE, 1, 0, 0, 0) == 0 && prctl(PR_SET_PTRACER, PR_SET_PTRACER_ANY, 0, 0, 0) == 0) {
        wrote = 1;
    }
#elif defined(SECCOMP)
    /* A filter that allows everything: harmless itself, but installed in a way the
       worker's filter does not allow. */
    struct sock_filter allow[] = { BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW) };
    struct sock_fprog prog = { 1, allow };
    if (syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, 0, &prog) == 0) {
        wrote = 1;
    }
#else
#error "compile with -DMARK=\"<path>\", -DPRCTL or -DSECCOMP"
#endif
}

int64_t initialiser_wrote(void) { return wrote; }
