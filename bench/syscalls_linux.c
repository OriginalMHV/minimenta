// Linux counterpart of syscalls.c: measures what each scan step costs per
// directory. Reports the median of 7 runs for 1 thread and for THREADS.
// The io_uring mode is built only when liburing is installed.
// Usage: syscalls_linux TREE THREADS
#define _GNU_SOURCE
#include <dirent.h>
#include <fcntl.h>
#include <fts.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
#if __has_include(<liburing.h>)
#include <liburing.h>
#define HAVE_URING 1
#endif

enum {
    OPEN_CLOSE,
    OPENAT_CLOSE,
    GETDENTS,
    GETDENTS_FSTATAT,
    GETDENTS_ONCE_FSTATAT,
    GETDENTS_STATX,
    OPENAT_GETDENTS_STATX,
    NCDU,
    URING_STATX,
    MODES
};
static const char *names[MODES] = {
    "open full path + close",
    "openat parent fd + close",
    "open + getdents64 to end (64 KiB buffer)",
    "open + getdents64 + fstatat per entry",
    "same, but skip the final empty getdents64",
    "open + getdents64 + statx (min mask) per entry",
    "openat parent + getdents64 + statx per entry",
    "ncdu-like: openat parent + readdir + fstatat",
    "open + getdents64 + io_uring statx batch",
};

struct dir {
    char *path;
    const char *name;
    int parent;
};

struct linux_dirent64 {
    uint64_t d_ino;
    int64_t d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[];
};

static struct dir *dirs;
static int *parent_fd;
static int ndirs, mode;
static atomic_int next;

#define STATX_MIN (STATX_TYPE | STATX_MODE | STATX_NLINK | STATX_INO | STATX_SIZE | STATX_BLOCKS)
#define BUF_WORDS 8192
#define URING_DEPTH 256

static int cmp_double(const void *a, const void *b) {
    double x = *(const double *)a, y = *(const double *)b;
    return (x > y) - (x < y);
}

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

static int skip(const char *n) {
    return n[0] == '.' && (n[1] == 0 || (n[1] == '.' && n[2] == 0));
}

#ifdef HAVE_URING
static __thread struct io_uring ring;
static __thread int ring_ready;
static __thread struct statx uring_sx[URING_DEPTH];

static void uring_stat(int fd, const char **batch, int n) {
    for (int k = 0; k < n; k++) {
        struct io_uring_sqe *sqe = io_uring_get_sqe(&ring);
        io_uring_prep_statx(sqe, fd, batch[k], AT_SYMLINK_NOFOLLOW | AT_STATX_DONT_SYNC, STATX_MIN,
                            &uring_sx[k]);
    }
    io_uring_submit_and_wait(&ring, n);
    struct io_uring_cqe *cqe;
    unsigned head, seen = 0;
    io_uring_for_each_cqe(&ring, head, cqe) seen++;
    io_uring_cq_advance(&ring, seen);
}
#endif

static void list(int fd, int how) {
    static __thread uint64_t buf[BUF_WORDS];
    const char *batch[URING_DEPTH];
    int queued = 0;
    struct stat st;
    struct statx sx;
    long n;
    while ((n = syscall(SYS_getdents64, fd, buf, sizeof buf)) > 0) {
        for (long off = 0; off < n;) {
            struct linux_dirent64 *e = (void *)((char *)buf + off);
            off += e->d_reclen;
            if (skip(e->d_name)) continue;
            if (how == GETDENTS_FSTATAT || how == GETDENTS_ONCE_FSTATAT)
                fstatat(fd, e->d_name, &st, AT_SYMLINK_NOFOLLOW);
            else if (how == GETDENTS_STATX)
                statx(fd, e->d_name, AT_SYMLINK_NOFOLLOW | AT_STATX_DONT_SYNC, STATX_MIN, &sx);
#ifdef HAVE_URING
            else if (how == URING_STATX) {
                batch[queued++] = e->d_name;
                if (queued == URING_DEPTH) {
                    uring_stat(fd, batch, queued);
                    queued = 0;
                }
            }
#endif
        }
#ifdef HAVE_URING
        // The names live in buf, so finish this batch before the next getdents64.
        if (queued) uring_stat(fd, batch, queued);
        queued = 0;
#endif
        // Measures the gain only. A short read does not prove the end of a
        // directory, so a scanner must not rely on it.
        if (how == GETDENTS_ONCE_FSTATAT && n < (long)(sizeof buf / 2)) break;
    }
    (void)batch;
    (void)queued;
}

static void *worker(void *arg) {
    (void)arg;
    const int flags = O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC;
#ifdef HAVE_URING
    if (mode == URING_STATX && !ring_ready) {
        if (io_uring_queue_init(URING_DEPTH, &ring, 0) < 0) return NULL;
        ring_ready = 1;
    }
#endif
    int i;
    while ((i = atomic_fetch_add(&next, 1)) < ndirs) {
        struct dir *d = &dirs[i];
        int relative = mode == OPENAT_CLOSE || mode == OPENAT_GETDENTS_STATX || mode == NCDU;
        int fd = relative && d->parent >= 0 ? openat(parent_fd[d->parent], d->name, flags)
                                            : open(d->path, flags);
        if (fd < 0) continue;
        switch (mode) {
        case OPEN_CLOSE:
        case OPENAT_CLOSE:
            break;
        case OPENAT_GETDENTS_STATX:
            list(fd, GETDENTS_STATX);
            break;
        case NCDU: {
            DIR *dp = fdopendir(fd);
            struct dirent *e;
            struct stat st;
            while ((e = readdir(dp)))
                if (!skip(e->d_name)) fstatat(fd, e->d_name, &st, AT_SYMLINK_NOFOLLOW);
            closedir(dp);
            continue;
        }
        default:
            list(fd, mode);
        }
        close(fd);
    }
#ifdef HAVE_URING
    if (ring_ready) {
        io_uring_queue_exit(&ring);
        ring_ready = 0;
    }
#endif
    return NULL;
}

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s TREE THREADS\n", argv[0]);
        return 2;
    }
    int threads = atoi(argv[2]);
    if (threads < 1 || threads > 64) threads = 64;
    char *roots[] = {argv[1], NULL};
    FTS *f = fts_open(roots, FTS_PHYSICAL | FTS_NOSTAT, NULL);
    FTSENT *e;
    int cap = 1 << 16;
    dirs = malloc(sizeof *dirs * cap);
    while ((e = fts_read(f))) {
        if (e->fts_info != FTS_D) continue;
        if (ndirs == cap) dirs = realloc(dirs, sizeof *dirs * (cap *= 2));
        // fts_number links each directory to its parent's index in dirs.
        e->fts_number = ndirs;
        int parent = e->fts_level > 0 ? (int)e->fts_parent->fts_number : -1;
        char *path = strdup(e->fts_path);
        char *slash = strrchr(path, '/');
        dirs[ndirs++] = (struct dir){path, slash ? slash + 1 : path, parent};
    }
    fts_close(f);

    // Keep every parent open, as a scanner that opens relative to the parent does.
    parent_fd = malloc(sizeof(int) * ndirs);
    for (int i = 0; i < ndirs; i++) parent_fd[i] = -1;
    for (int i = 0; i < ndirs; i++) {
        int p = dirs[i].parent;
        if (p >= 0 && parent_fd[p] < 0)
            parent_fd[p] = open(dirs[p].path, O_RDONLY | O_DIRECTORY | O_CLOEXEC);
        if (p >= 0 && parent_fd[p] < 0) {
            perror("open parent (raise ulimit -n)");
            return 1;
        }
    }

    printf("%d directories\n", ndirs);
    for (int t = 1;; t = threads) {
        for (mode = 0; mode < MODES; mode++) {
#ifndef HAVE_URING
            if (mode == URING_STATX) continue;
#endif
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
            printf("%2d thread(s)  %-48s %7.1f ms  %6.1f us/dir\n", t, names[mode], median * 1e3,
                   median / ndirs * 1e6);
        }
        if (t == threads) break;
    }
    return 0;
}
