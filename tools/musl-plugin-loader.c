#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: %s PLUGIN.so\n", argv[0]);
        return 2;
    }

    void *handle = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (handle == NULL) {
        fprintf(stderr, "dlopen(%s): %s\n", argv[1], dlerror());
        return 1;
    }
    dlerror();
    void *register_symbol = dlsym(handle, "rpi_plugin_register");
    const char *error = dlerror();
    if (error != NULL || register_symbol == NULL) {
        fprintf(stderr, "dlsym(rpi_plugin_register): %s\n",
                error == NULL ? "symbol is null" : error);
        dlclose(handle);
        return 1;
    }
    if (dlclose(handle) != 0) {
        fprintf(stderr, "dlclose(%s): %s\n", argv[1], dlerror());
        return 1;
    }
    return 0;
}
