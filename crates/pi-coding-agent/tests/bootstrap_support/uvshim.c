#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void log_args(int argc, char** argv) {
    const char* log = getenv("PARITY_UV_LOG");
    if (!log) return;
    FILE* f = fopen(log, "a");
    if (!f) return;
    for (int i = 1; i < argc; i++) {
        fputs(argv[i], f);
        if (i + 1 < argc) fputc(' ', f);
    }
    fputc('\n', f);
    fclose(f);
}

int main(int argc, char** argv) {
    log_args(argc, argv);
    const char* sleep_ms = getenv("PARITY_UV_SLEEP_MS");
    if (sleep_ms && *sleep_ms) Sleep((DWORD)atoi(sleep_ms));
    const char* fail_path = getenv("PARITY_UV_FAIL_PATH");
    if (fail_path && *fail_path && argc >= 3) {
        for (int i = 2; i < argc; i++) {
            if (strcmp(argv[i], fail_path) == 0) return 1;
        }
    }
    if (argc >= 4 && strcmp(argv[1], "venv") == 0) {
        /* mkdir -p the venv directory itself, then Scripts inside it. */
        char buf[MAX_PATH];
        snprintf(buf, sizeof(buf), "%s", argv[2]);
        for (char* p = buf + 3; *p; p++) {
            if (*p == '\\' || *p == '/') {
                char saved = *p;
                *p = 0;
                CreateDirectoryA(buf, NULL);
                *p = saved;
            }
        }
        CreateDirectoryA(buf, NULL);
        char scripts[MAX_PATH];
        snprintf(scripts, sizeof(scripts), "%s\\Scripts", argv[2]);
        CreateDirectoryA(scripts, NULL);
        const char* noop = getenv("PARITY_NOOP_PYTHON");
        char target[MAX_PATH];
        snprintf(target, sizeof(target), "%s\\python.exe", scripts);
        if (noop) CopyFileA(noop, target, FALSE);
        return 0;
    }
    if (argc >= 2 && (strcmp(argv[1], "python") == 0 || strcmp(argv[1], "pip") == 0)) return 0;
    return 1;
}
