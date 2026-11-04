csrc = []

if host_machine.cpu_family() == 'aarch64'
    extra_args += '--cfg'
    extra_args += 'blake3_neon'
    csrc += ['../c/blake3_neon.c']
elif host_machine.cpu_family() == 'x86_64'
    extra_args += '--cfg'
    extra_args += 'blake3_sse2_ffi'
    extra_args += '--cfg'
    extra_args += 'blake3_sse41_ffi'
    extra_args += '--cfg'
    extra_args += 'blake3_avx2_ffi'
    csrc += [
        '../c/blake3_sse2_x86-64_unix.S',
        '../c/blake3_sse41_x86-64_unix.S',
        '../c/blake3_avx2_x86-64_unix.S',
    ]
endif
add_languages('c')
extra_args += '-l'
extra_args += 'static=blake3'
extra_args += '-L'
extra_args += meson.current_build_dir()

blake3_lib = static_library('blake3', csrc, install: false)

extra_deps = declare_dependency(link_whole: blake3_lib)
