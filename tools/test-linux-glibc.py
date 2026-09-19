#!/usr/bin/env python3
"""Reproduce host-driver dlopen with a GLIBC_2.43 libm requirement.

Usage: test-linux-glibc.py OLD_GAME FIXED_GAME OUTPUT_DIRECTORY
Requires a C compiler. Uses each game's real bundled loader/library directory.
"""
from pathlib import Path
import subprocess
import sys

old, new, output = (Path(p).resolve() for p in sys.argv[1:])
output.mkdir(parents=True, exist_ok=True)
(output / "driver.c").write_text('''
extern float modern_atanhf(float);
__asm__(".symver modern_atanhf,atanhf@GLIBC_2.43");
float driver_value(void) { return modern_atanhf(0.5f); }
''')
(output / "probe.c").write_text('''
#include <dlfcn.h>
#include <stdio.h>
int main(int argc, char **argv) {
    if (argc != 2) return 3;
    void *driver = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!driver) { fprintf(stderr, "%s\\n", dlerror()); return 1; }
    float (*value)(void) = dlsym(driver, "driver_value");
    if (!value) return 2;
    float result = value();
    printf("GLIBC_2.43 driver loaded: %.6f\\n", result);
    dlclose(driver);
    return result > 0.549f && result < 0.550f ? 0 : 2;
}
''')
subprocess.run(["cc", "-shared", "-fPIC", output / "driver.c",
                new / "lib/libm.so.6", "-o", output / "driver.so"], check=True)
subprocess.run(["cc", output / "probe.c", "-ldl", "-o", output / "probe"], check=True)
for name, game in (("old", old), ("fixed", new)):
    result = subprocess.run([game / "lib/ld-linux-x86-64.so.2", "--library-path",
                             game / "lib", output / "probe", output / "driver.so"],
                            text=True, capture_output=True)
    log = result.stdout + result.stderr
    (output / (name + ".log")).write_text(log)
    print(name + ": " + log.strip())
    if name == "old":
        assert result.returncode != 0 and "GLIBC_2.43" in log and "not found" in log
    else:
        assert result.returncode == 0 and "GLIBC_2.43 driver loaded" in log
