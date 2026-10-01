/* Inject a Darwin ttyname_r device-lookup failure into the real client. */
#include <errno.h>
#include <unistd.h>

#ifdef RUSTMUX_TTYNAME_PROBE
#include <util.h>

int main(void) {
    int master, slave;
    char path[1024];
    if (openpty(&master, &slave, NULL, NULL, NULL) != 0) return 2;
    int result = ttyname_r(slave, path, sizeof(path));
    close(master);
    close(slave);
    if (result == ERANGE) return 0;
    return result == 0 ? 1 : 3;
}
#else
static int fail_ttyname(int fd, char *path, size_t size) {
    (void)fd;
    (void)path;
    (void)size;
    return ERANGE;
}

__attribute__((used, section("__DATA,__interpose")))
static const struct {
    const void *replacement;
    const void *original;
} interpose = { (const void *)fail_ttyname, (const void *)ttyname_r };
#endif
