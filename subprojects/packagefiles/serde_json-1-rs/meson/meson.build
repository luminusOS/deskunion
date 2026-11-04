extra_args += '--check-cfg'
extra_args += 'cfg(fast_arithmetic, values("32", "64"))'
extra_args += '--cfg'
if ['aarch64', 'loongarch64', 'mips64', 'ppc64', 'wasm32', 'x86_64'].contains(
    host_machine.cpu_family(),
)
    extra_args += 'fast_arithmetic="64"'
else
    extra_args += 'fast_arithmetic="32"'
endif
