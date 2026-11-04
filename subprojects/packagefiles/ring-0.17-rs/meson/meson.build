add_languages('c')
extra_args += '-l'
extra_args += 'static=ring'
extra_args += '-L'
extra_args += meson.current_build_dir()

csrc = []

if host_machine.cpu_family() == 'aarch64'
    csrc += [
        '../pregenerated/chacha-armv8-linux64.S',
        '../pregenerated/chacha20_poly1305_armv8-linux64.S',
        '../pregenerated/aesv8-armx-linux64.S',
        '../pregenerated/aesv8-gcm-armv8-linux64.S',
        '../pregenerated/ghash-neon-armv8-linux64.S',
        '../pregenerated/ghashv8-armx-linux64.S',
        '../pregenerated/vpaes-armv8-linux64.S',
        '../pregenerated/armv8-mont-linux64.S',
        '../pregenerated/p256-armv8-asm-linux64.S',
        '../pregenerated/sha512-armv8-linux64.S',
        '../pregenerated/sha256-armv8-linux64.S',
    ]
elif host_machine.cpu_family() == 'x86_64'
    csrc += [
        '../crypto/crypto.c',
        '../crypto/cpu_intel.c',
        '../crypto/curve25519/curve25519_64_adx.c',
        '../third_party/fiat/asm/fiat_curve25519_adx_mul.S',
        '../third_party/fiat/asm/fiat_curve25519_adx_square.S',
        '../pregenerated/chacha-x86_64-elf.S',
        '../pregenerated/aes-gcm-avx2-x86_64-elf.S',
        '../pregenerated/aesni-gcm-x86_64-elf.S',
        '../pregenerated/aesni-x86_64-elf.S',
        '../pregenerated/ghash-x86_64-elf.S',
        '../pregenerated/vpaes-x86_64-elf.S',
        '../pregenerated/x86_64-mont-elf.S',
        '../pregenerated/x86_64-mont5-elf.S',
        '../pregenerated/p256-x86_64-asm-elf.S',
        '../pregenerated/sha512-x86_64-elf.S',
        '../pregenerated/chacha20_poly1305_x86_64-elf.S',
        '../pregenerated/sha256-x86_64-elf.S',
    ]
endif

ring_lib = static_library(
    'ring',
    '../crypto/curve25519/curve25519.c',
    '../crypto/fipsmodule/aes/aes_nohw.c',
    '../crypto/fipsmodule/bn/montgomery.c',
    '../crypto/fipsmodule/bn/montgomery_inv.c',
    '../crypto/fipsmodule/ec/ecp_nistz.c',
    '../crypto/fipsmodule/ec/gfp_p256.c',
    '../crypto/fipsmodule/ec/gfp_p384.c',
    '../crypto/fipsmodule/ec/p256.c',
    '../crypto/limbs/limbs.c',
    '../crypto/mem.c',
    '../crypto/poly1305/poly1305.c',
    '../crypto/fipsmodule/ec/p256-nistz.c',
    csrc,
    c_args: [
        '-DNDEBUG',
        '-I' + meson.current_source_dir() + '/../pregenerated',
        '-I' + meson.current_source_dir() + '/../include',
    ],
    install: false,
)

extra_deps = declare_dependency(link_whole: ring_lib)
