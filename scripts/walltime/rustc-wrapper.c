// Records an invocation identity, then execs rustc without adding a wait process.
// PR_SET_NAME correlates container PIDs with the kernel's host TIDs in trace.bt.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <time.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc < 2) return 2;
    const char *dir = getenv("SL7_WALLTIME_UNITS");
    if (dir) {
        const char *name = "", *type = "", *extra = "";
        for (int i = 2; i < argc; i++) {
            if (i + 1 < argc && strcmp(argv[i], "--crate-name") == 0) name = argv[++i];
            else if (i + 1 < argc && strcmp(argv[i], "--crate-type") == 0) type = argv[++i];
            else if (strncmp(argv[i], "extra-filename=", 15) == 0) extra = argv[i] + 15;
        }
        char marker[16], path[4096];
        snprintf(marker, sizeof(marker), "sl7u_%d", getpid());
        if (prctl(PR_SET_NAME, marker, 0, 0, 0) != 0) return 3;
        int length = snprintf(path, sizeof(path), "%s/%d.tsv", dir, getpid());
        if (length < 0 || (size_t)length >= sizeof(path)) return 4;
        int fd = open(path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600);
        if (fd < 0) return 5;
        struct timespec ts;
        if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) return 6;
        const char *package = getenv("CARGO_PKG_NAME");
        const char *version = getenv("CARGO_PKG_VERSION");
        int result = dprintf(fd, "%lld\t%d\t%s\t%s\t%s\t%s\t%s\t%d\n",
                            (long long)ts.tv_sec * 1000000000 + ts.tv_nsec,
                            getpid(), name, type, extra,
                            package ? package : "", version ? version : "",
                            getenv("CARGO_PRIMARY_PACKAGE") != NULL);
        if (close(fd) != 0 || result < 0) return 7;
    }
    execvp(argv[1], argv + 1);
    perror("exec rustc");
    return 127;
}
