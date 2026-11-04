project(
    'deskunion',
    'rust',
    version: '0.2.11',
    meson_version: '>= 1.12.0',
    default_options: [
        'warning_level=2',
        'werror=false',
    ],
)

platform = get_option('platform')
release_type = get_option('release-type')

gnome = import('gnome')

deskunion_deps = []
deskunion_deps += dependency('gtk4', version: '>= 4.14')
deskunion_deps += dependency('libadwaita-1', version: '>= 1.5')

application_id = 'io.github.luminusos.DeskUnion'
application_name = 'DeskUnion'

prefix = get_option('prefix')
bindir = prefix / get_option('bindir')
datadir = prefix / get_option('datadir')
iconsdir = datadir / 'icons'

install_data(
    'io.github.luminusos.DeskUnion.metainfo.xml',
    install_dir: datadir / 'metainfo',
)

install_data(
    'io.github.luminusos.DeskUnion.desktop',
    install_dir: datadir / 'applications',
)

install_data(
    'crates/deskunion-gtk/resources/io.github.luminusos.DeskUnion.svg',
    install_dir: iconsdir / 'hicolor' / 'scalable' / 'apps',
)

install_data(
    'crates/deskunion-gtk/resources/io.github.luminusos.DeskUnion-symbolic.svg',
    install_dir: iconsdir / 'hicolor' / 'symbolic' / 'apps',
)

features = ['gtk']

cargo_module = import('rust').workspace(
    default_features: false,
    features: features,
)
crate = cargo_module.package('deskunion-app')
crate.executable(
    'deskunion',
    install: true,
    android_exe_type: 'application',
    dependencies: deskunion_deps,
)

gnome.post_install(
    gtk_update_icon_cache: true,
    update_desktop_database: true,
)
