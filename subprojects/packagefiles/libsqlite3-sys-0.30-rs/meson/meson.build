cfg = configuration_data()
bindgen = configure_file(
    configuration: cfg,
    input: 'bindgen.rs.in',
    output: 'bindgen.rs',
)

# Output of build.rs in x86, no android
# running: LC_ALL="C" "cc" "-O0" "-ffunction-sections" "-fdata-sections" "-fPIC" "-m64" "-w" "-DSQLITE_CORE" "-DSQLITE_DEFAULT_FOREIGN_KEYS=1" "-DSQLITE_ENABLE_API_ARMOR" "-DSQLITE_ENABLE_COLUMN_METADATA" "-DSQLITE_ENABLE_DBSTAT_VTAB" "-DSQLITE_ENABLE_FTS3" "-DSQLITE_ENABLE_FTS3_PARENTHESIS" "-DSQLITE_ENABLE_FTS5" "-DSQLITE_ENABLE_JSON1" "-DSQLITE_ENABLE_LOAD_EXTENSION=1" "-DSQLITE_ENABLE_MEMORY_MANAGEMENT" "-DSQLITE_ENABLE_RTREE" "-DSQLITE_ENABLE_STAT4" "-DSQLITE_SOUNDEX" "-DSQLITE_THREADSAFE=1" "-DSQLITE_USE_URI" "-DHAVE_USLEEP=1" "-DHAVE_ISNAN" "-D_POSIX_THREAD_SAFE_FUNCTIONS" "-DHAVE_LOCALTIME_R" "-DSQLITE_ENABLE_UNLOCK_NOTIFY" "-o" "tmp_x86/c877a2978823c39d-sqlite3.o" "-c" "sqlite3/sqlite3.c"
#
#
# Output of build.rs in x86, android
# running: LC_ALL="C" "/opt/android/ndk/29.0.14206865/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android35-clang" "-O0" "-DANDROID" "-ffunction-sections" "-fdata-sections" "-fPIC" "-m64" "-w" "-DSQLITE_CORE" "-DSQLITE_DEFAULT_FOREIGN_KEYS=1" "-DSQLITE_ENABLE_API_ARMOR" "-DSQLITE_ENABLE_COLUMN_METADATA" "-DSQLITE_ENABLE_DBSTAT_VTAB" "-DSQLITE_ENABLE_FTS3" "-DSQLITE_ENABLE_FTS3_PARENTHESIS" "-DSQLITE_ENABLE_FTS5" "-DSQLITE_ENABLE_JSON1" "-DSQLITE_ENABLE_LOAD_EXTENSION=1" "-DSQLITE_ENABLE_MEMORY_MANAGEMENT" "-DSQLITE_ENABLE_RTREE" "-DSQLITE_ENABLE_STAT4" "-DSQLITE_SOUNDEX" "-DSQLITE_THREADSAFE=1" "-DSQLITE_USE_URI" "-DHAVE_USLEEP=1" "-DHAVE_ISNAN" "-D_POSIX_THREAD_SAFE_FUNCTIONS" "-DSQLITE_TEMP_STORE=3" "-DHAVE_LOCALTIME_R" "-DSQLITE_ENABLE_UNLOCK_NOTIFY" "-o" "tmp_x86/c877a2978823c39d-sqlite3.o" "-c" "sqlite3/sqlite3.c"
# The difference is -DANDROID -DSQLITE_TEMP_STORE

cargs = []

if host_machine.system() == 'android'
    cargs += ['-DANDROID', '-DSQLITE_TEMP_STORE=3']
endif

add_languages('c')
extra_args += '-l'
extra_args += 'static=sqlite3'
extra_args += '-L'
extra_args += meson.current_build_dir()

sqlite3_lib = static_library(
    'sqlite3',
    '../sqlite3/sqlite3.c',
    c_args: [
        '-DSQLITE_CORE',
        '-DSQLITE_DEFAULT_FOREIGN_KEYS=1',
        '-DSQLITE_ENABLE_API_ARMOR',
        '-DSQLITE_ENABLE_COLUMN_METADATA',
        '-DSQLITE_ENABLE_DBSTAT_VTAB',
        '-DSQLITE_ENABLE_FTS3',
        '-DSQLITE_ENABLE_FTS3_PARENTHESIS',
        '-DSQLITE_ENABLE_FTS5',
        '-DSQLITE_ENABLE_JSON1',
        '-DSQLITE_ENABLE_LOAD_EXTENSION=1',
        '-DSQLITE_ENABLE_MEMORY_MANAGEMENT',
        '-DSQLITE_ENABLE_RTREE',
        '-DSQLITE_ENABLE_STAT4',
        '-DSQLITE_SOUNDEX',
        '-DSQLITE_THREADSAFE=1',
        '-DSQLITE_USE_URI',
        '-DHAVE_USLEEP=1',
        '-DHAVE_ISNAN',
        '-D_POSIX_THREAD_SAFE_FUNCTIONS',
        '-DHAVE_LOCALTIME_R',
        '-DSQLITE_ENABLE_UNLOCK_NOTIFY',
    ],
    install: false,
)

extra_deps = declare_dependency(link_whole: sqlite3_lib)
