/* The bundle root is the physical executable directory, independent of cwd. */
int main(void) {
    char *executable = NULL;
#ifdef __APPLE__
    uint32_t size = 0;
    (void)_NSGetExecutablePath(NULL, &size);
    char *path = malloc(size);
    if (path != NULL && _NSGetExecutablePath(path, &size) == 0) {
        executable = realpath(path, NULL);
    }
    free(path);
#elif defined(__linux__)
    executable = realpath("/proc/self/exe", NULL);
#else
#error Project executables currently require macOS or Linux
#endif
    if (executable == NULL) {
        fputs("Cannot locate the Joky application executable\n", stderr);
        return 1;
    }
    char *separator = strrchr(executable, '/');
    if (separator == NULL) {
        free(executable);
        fputs("Cannot locate the Joky application directory\n", stderr);
        return 1;
    }
    separator[separator == executable ? 1 : 0] = '\0';
    int status = chdir(executable);
    free(executable);
    if (status != 0) {
        perror("Cannot enter the Joky application directory");
        return 1;
    }
    return joky_project_main();
}
