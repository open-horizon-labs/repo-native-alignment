#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/landlock.h>
#include <linux/prctl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef LANDLOCK_ACCESS_FS_REFER
#define LANDLOCK_ACCESS_FS_REFER (1ULL << 13)
#endif
#ifndef LANDLOCK_ACCESS_FS_TRUNCATE
#define LANDLOCK_ACCESS_FS_TRUNCATE (1ULL << 14)
#endif

static int ll_create(const struct landlock_ruleset_attr *attr, size_t size,
                     __u32 flags) {
    return syscall(SYS_landlock_create_ruleset, attr, size, flags);
}

static int ll_add(int fd, const struct landlock_path_beneath_attr *rule) {
    return syscall(SYS_landlock_add_rule, fd, LANDLOCK_RULE_PATH_BENEATH,
                   rule, 0);
}

static int ll_restrict(int fd) {
    return syscall(SYS_landlock_restrict_self, fd, 0);
}

static __u64 read_rights(void) {
    return LANDLOCK_ACCESS_FS_EXECUTE | LANDLOCK_ACCESS_FS_READ_FILE |
           LANDLOCK_ACCESS_FS_READ_DIR;
}

static __u64 write_rights(int abi) {
    __u64 rights = read_rights() | LANDLOCK_ACCESS_FS_WRITE_FILE |
        LANDLOCK_ACCESS_FS_REMOVE_DIR | LANDLOCK_ACCESS_FS_REMOVE_FILE |
        LANDLOCK_ACCESS_FS_MAKE_CHAR | LANDLOCK_ACCESS_FS_MAKE_DIR |
        LANDLOCK_ACCESS_FS_MAKE_REG | LANDLOCK_ACCESS_FS_MAKE_SOCK |
        LANDLOCK_ACCESS_FS_MAKE_FIFO | LANDLOCK_ACCESS_FS_MAKE_BLOCK |
        LANDLOCK_ACCESS_FS_MAKE_SYM;
    if (abi >= 2) rights |= LANDLOCK_ACCESS_FS_REFER;
    if (abi >= 3) rights |= LANDLOCK_ACCESS_FS_TRUNCATE;
    return rights;
}

static void add_path(int ruleset, const char *path, __u64 rights) {
    int parent = open(path, O_PATH | O_CLOEXEC);
    if (parent < 0) {
        fprintf(stderr, "landlock open %s: %s\n", path, strerror(errno));
        exit(72);
    }
    struct landlock_path_beneath_attr rule = {
        .allowed_access = rights,
        .parent_fd = parent,
    };
    if (ll_add(ruleset, &rule) < 0) {
        fprintf(stderr, "landlock add %s: %s\n", path, strerror(errno));
        close(parent);
        exit(73);
    }
    close(parent);
}

int main(int argc, char **argv) {
    int abi = ll_create(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION);
    if (abi < 0) {
        fprintf(stderr, "landlock ABI unavailable: %s\n", strerror(errno));
        return 70;
    }
    if (argc == 2 && strcmp(argv[1], "--abi") == 0) {
        printf("%d\n", abi);
        return 0;
    }

    int required = 1;
    int self_test = 0;
    const char *deny_probe = NULL;
    int separator = -1;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--require-abi") == 0 && i + 1 < argc) {
            required = atoi(argv[++i]);
        } else if (strcmp(argv[i], "--self-test-json") == 0) {
            self_test = 1;
        } else if (strcmp(argv[i], "--deny-probe") == 0 && i + 1 < argc) {
            deny_probe = argv[++i];
        } else if (strcmp(argv[i], "--ro") == 0 && i + 1 < argc) {
            i++;
        } else if (strcmp(argv[i], "--rw") == 0 && i + 1 < argc) {
            i++;
        } else if (strcmp(argv[i], "--") == 0) {
            separator = i;
            break;
        } else {
            fprintf(stderr, "invalid launcher argument\n");
            return 64;
        }
    }
    if (abi < required || (!self_test && separator < 0) || !deny_probe) {
        fprintf(stderr, "Landlock requirement or command missing\n");
        return 65;
    }

    __u64 handled = write_rights(abi);
    struct landlock_ruleset_attr attr = {.handled_access_fs = handled};
    int ruleset = ll_create(&attr, sizeof(attr), 0);
    if (ruleset < 0) {
        fprintf(stderr, "landlock create: %s\n", strerror(errno));
        return 71;
    }
    for (int i = 1; i < argc && i != separator; i++) {
        if (strcmp(argv[i], "--ro") == 0 && i + 1 < argc) {
            add_path(ruleset, argv[++i], read_rights());
        } else if (strcmp(argv[i], "--rw") == 0 && i + 1 < argc) {
            add_path(ruleset, argv[++i], handled);
        } else if ((strcmp(argv[i], "--require-abi") == 0 ||
                    strcmp(argv[i], "--deny-probe") == 0) && i + 1 < argc) {
            i++;
        }
    }
    if (prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0 ||
        ll_restrict(ruleset) < 0) {
        fprintf(stderr, "landlock enforce: %s\n", strerror(errno));
        close(ruleset);
        return 74;
    }
    close(ruleset);

    errno = 0;
    int denied = open(deny_probe, O_RDONLY | O_CLOEXEC);
    if (denied >= 0 || errno != EACCES) {
        if (denied >= 0) close(denied);
        fprintf(stderr, "deny probe did not fail closed: errno=%d\n", errno);
        return 75;
    }
    if (self_test) {
        printf("{\"abi\":%d,\"denied_probe\":true,\"enforced\":true}\n", abi);
        return 0;
    }
    execvp(argv[separator + 1], &argv[separator + 1]);
    fprintf(stderr, "exec: %s\n", strerror(errno));
    return 76;
}
