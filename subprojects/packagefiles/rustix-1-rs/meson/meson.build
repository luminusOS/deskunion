if host_machine.system() == 'android'
    extra_args += '--cfg'
    extra_args += 'libc'

    extra_args += '--cfg'
    extra_args += 'linux_raw_dep'
elif host_machine.system() == 'linux'
    extra_args += '--cfg'
    extra_args += 'linux_raw_dep'

    extra_args += '--cfg'
    extra_args += 'linux_raw'
else
    error('Unsupported OS: @0@'.format(host_machine.system()))
endif


if ['linux', 'android'].contains(host_machine.system())
    extra_args += '--cfg'
    extra_args += 'linux_like'

    extra_args += '--cfg'
    extra_args += 'linux_kernel'
endif
