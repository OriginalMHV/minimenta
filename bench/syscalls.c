// Measures what each scan step costs per directory: open, list (with growing
// attribute sets), close, and per-file fstatat. Reports the median of 7 runs.
// Usage: syscalls TREE THREADS
#include <sys/attr.h>
#include <sys/stat.h>
#include <dirent.h>
#include <fcntl.h>
#include <fts.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

enum { OPEN_CLOSE, LIST_NAMES, LIST_SIZES, LIST_FULL, LIST_TO_END, READDIR_FSTATAT, MODES };
static const char *names[MODES] = {
    "open + close",
    "list x1: name, type",
    "list x1: + alloc size, data length",
    "list x1: + inode, device, link count (all)",
    "list to end: all attributes",
    "opendir + readdir + fstatat per file (ncdu)",
};
static char **dirs;
static int ndirs, mode;
static atomic_int next;

static int cmp_double(const void *a, const void *b) {
    double x = *(const double *)a, y = *(const double *)b;
    return (x > y) - (x < y);
}

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static void *worker(void *arg) {
    (void)arg;
    static __thread uint64_t buf[32768];
    struct attrlist a = {ATTR_BIT_MAP_COUNT, 0, ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_OBJTYPE | ATTR_CMN_ERROR, 0, 0, 0, 0};
    if (mode >= LIST_SIZES) a.fileattr = ATTR_FILE_ALLOCSIZE | ATTR_FILE_DATALENGTH;
    if (mode >= LIST_FULL) {
        a.commonattr |= ATTR_CMN_DEVID | ATTR_CMN_FILEID;
        a.dirattr = ATTR_DIR_ENTRYCOUNT | ATTR_DIR_MOUNTSTATUS;
        a.fileattr |= ATTR_FILE_LINKCOUNT;
    }
    int i;
    while ((i = atomic_fetch_add(&next, 1)) < ndirs) {
        if (mode == READDIR_FSTATAT) {
            DIR *d = opendir(dirs[i]);
            struct dirent *e;
            struct stat st;
            while ((e = readdir(d)))
                if (strcmp(e->d_name, ".") && strcmp(e->d_name, ".."))
                    fstatat(dirfd(d), e->d_name, &st, AT_SYMLINK_NOFOLLOW);
            closedir(d);
            continue;
        }
        int fd = open(dirs[i], O_RDONLY | O_DIRECTORY);
        if (mode >= LIST_NAMES && mode <= LIST_FULL)
            getattrlistbulk(fd, &a, buf, sizeof buf, FSOPT_PACK_INVAL_ATTRS);
        if (mode == LIST_TO_END)
            while (getattrlistbulk(fd, &a, buf, sizeof buf, FSOPT_PACK_INVAL_ATTRS) > 0) {}
        close(fd);
    }
    return NULL;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s TREE THREADS\n", argv[0]);
        return 2;
    }
    int threads = atoi(argv[2]);
    char *roots[] = {argv[1], NULL};
    FTS *f = fts_open(roots, FTS_PHYSICAL | FTS_NOSTAT, NULL);
    FTSENT *e;
    dirs = malloc(sizeof(char *) * 1000000);
    while ((e = fts_read(f)))
        if (e->fts_info == FTS_D) dirs[ndirs++] = strdup(e->fts_path);
    for (int t = 1; t <= threads; t += threads - 1 ? threads - 1 : 1) {
        for (mode = 0; mode < MODES; mode++) {
            double times[7];
            for (int r = 0; r < 7; r++) {
                pthread_t tid[64];
                next = 0;
                double s = now();
                for (int k = 0; k < t; k++) pthread_create(&tid[k], NULL, worker, NULL);
                for (int k = 0; k < t; k++) pthread_join(tid[k], NULL);
                times[r] = now() - s;
            }
            qsort(times, 7, sizeof(double), cmp_double);
            double median = times[3];
            printf("%2d thread(s)  %-45s %7.1f ms  %6.1f us/dir\n", t, names[mode], median * 1e3, median / ndirs * 1e6);
        }
        if (threads == 1) break;
    }
    return 0;
}
