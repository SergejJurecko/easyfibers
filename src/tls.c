#include <stddef.h>

__thread void* runner = NULL;
void *get_runner() {
    return runner;
}

void set_runner(void* r) {
    runner = r;
}