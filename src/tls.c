#include <stddef.h>
#include <stdint.h>

#define MAX_RUNNERS 20

__thread void* poller = NULL;
__thread void* runner[MAX_RUNNERS] = 
    {NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
    NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL};

int8_t find_empty() {
    int8_t i;
    for (i = 0; i < MAX_RUNNERS; i++) {
        if (runner[i] == NULL) {
            return i;
        }
    }
    return -1;
}

void *get_poller() {
    return poller;
}

void set_poller(void* r) {
    poller = r;
}

void *get_runner(int8_t pos) {
    return runner[pos];
}

void set_runner(int8_t pos, void* r) {
    runner[pos] = r;
}