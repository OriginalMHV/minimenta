// Measures what each scan step costs per directory: open, list, close, and
// per-file fstatat. Usage: syscalls TREE THREADS
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

enum { OPEN_CLOSE, LIST_ONCE, LIST_TO_END, READDIR_FSTATAT, MODES };
static const char *names[MODES] = {
    "open + close",
    "open + getattrlistbulk x1 + close",
    "open + getattrlistbulk to end + close",
    "opendir + readdir + fstatat per file (ncdu)",
};
static char **dirs;
static int ndirs, mode;
static atomic_int next;

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static void *worker(void *arg) {
    (void)arg;
    static __thread uint64_t buf[32768];
    struct attrlist a = {ATTR_BIT_MAP_COUNT, 0,
        ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_DEVID | ATTR_CMN_OBJTYPE | ATTR_CMN_FILEID | ATTR_CMN_ERROR,
        0, ATTR_DIR_MOUNTSTATUS, ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE | ATTR_FILE_DATALENGTH, 0};
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
        if (mode == LIST_ONCE)
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
            double best = 1e9;
            for (int r = 0; r < 7; r++) {
                pthread_t tid[64];
                next = 0;
                double s = now();
                for (int k = 0; k < t; k++) pthread_create(&tid[k], NULL, worker, NULL);
                for (int k = 0; k < t; k++) pthread_join(tid[k], NULL);
                double d = now() - s;
                if (d < best) best = d;
            }
            printf("%2d thread(s)  %-45s %7.1f ms  %6.1f us/dir\n", t, names[mode], best * 1e3, best / ndirs * 1e6);
        }
        if (threads == 1) break;
    }
    return 0;
}
