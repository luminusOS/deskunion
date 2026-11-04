cfg = configuration_data()
cfg.set('VERSION_PATCH', meson.project_version().split('.')[2])

priv = configure_file(
    configuration: cfg,
    input: 'private.rs.in',
    output: 'private.rs',
)
