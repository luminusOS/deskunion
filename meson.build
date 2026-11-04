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
pkg_input_event = cargo_module.package('input-event')
lib_input_event = pkg_input_event.library()
pkg_input_event.override_dependency(declare_dependency(link_with: lib_input_event))

pkg_deskunion_proto = cargo_module.package('deskunion-proto')
lib_deskunion_proto = pkg_deskunion_proto.library()
pkg_deskunion_proto.override_dependency(declare_dependency(link_with: lib_deskunion_proto))

pkg_deskunion_ipc = cargo_module.package('deskunion-ipc')
lib_deskunion_ipc = pkg_deskunion_ipc.library()
pkg_deskunion_ipc.override_dependency(declare_dependency(link_with: lib_deskunion_ipc))

pkg_input_emulation = cargo_module.package('input-emulation')
lib_input_emulation = pkg_input_emulation.library()
pkg_input_emulation.override_dependency(declare_dependency(link_with: lib_input_emulation))

pkg_input_capture = cargo_module.package('input-capture')
lib_input_capture = pkg_input_capture.library()
pkg_input_capture.override_dependency(declare_dependency(link_with: lib_input_capture))

pkg_deskunion_cli = cargo_module.package('deskunion-cli')
lib_deskunion_cli = pkg_deskunion_cli.library()
pkg_deskunion_cli.override_dependency(declare_dependency(link_with: lib_deskunion_cli))

pkg_deskunion_gtk = cargo_module.package('deskunion-gtk')
lib_deskunion_gtk = pkg_deskunion_gtk.library()
pkg_deskunion_gtk.override_dependency(declare_dependency(link_with: lib_deskunion_gtk))

crate = cargo_module.package('deskunion-app')
lib_deskunion = crate.library('deskunion_lib', dependencies: deskunion_deps)
crate.executable(
    'deskunion',
    install: true,
    android_exe_type: 'application',
    dependencies: deskunion_deps,
    link_with: lib_deskunion,
    rust_dependency_map: {'deskunion_lib': 'deskunion'},
)

gnome.post_install(
    gtk_update_icon_cache: true,
    update_desktop_database: true,
)
