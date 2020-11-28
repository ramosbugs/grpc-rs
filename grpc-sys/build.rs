// Copyright 2019 TiKV Project Authors. Licensed under Apache-2.0.

use std::env::VarError;
use std::io::prelude::*;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::{env, fs, io};

use cmake::Config as CmakeConfig;
use pkg_config::{Config as PkgConfig, Library};
use walkdir::WalkDir;

const GRPC_VERSION: &str = "1.29.1";

fn probe_library(library: &str, cargo_metadata: bool) -> Library {
    match PkgConfig::new()
        .atleast_version(GRPC_VERSION)
        .cargo_metadata(cargo_metadata)
        .probe(library)
    {
        Ok(lib) => lib,
        Err(e) => panic!("can't find library {} via pkg-config: {:?}", library, e),
    }
}

fn prepare_grpc() {
    let mut modules = vec![
        "grpc",
        "grpc/third_party/cares/cares",
        "grpc/third_party/address_sorting",
        "grpc/third_party/abseil-cpp",
    ];

    if cfg!(feature = "secure") && !cfg!(feature = "openssl") {
        modules.push("grpc/third_party/boringssl-with-bazel");
    }

    for module in modules {
        if is_directory_empty(module).unwrap_or(true) {
            panic!(
                "Can't find module {}. You need to run `git submodule \
                 update --init --recursive` first to build the project.",
                module
            );
        }
    }
}

fn is_directory_empty<P: AsRef<Path>>(p: P) -> Result<bool, io::Error> {
    let mut entries = fs::read_dir(p)?;
    Ok(entries.next().is_none())
}

fn trim_start<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.starts_with(prefix) {
        Some(s.trim_start_matches(prefix))
    } else {
        None
    }
}

fn build_grpc(cc: &mut cc::Build, library: &str) {
    prepare_grpc();

    let mut third_party = vec![
        "cares/cares/lib",
        "abseil-cpp/absl/strings",
        "abseil-cpp/absl/time",
        "abseil-cpp/absl/base",
        "abseil-cpp/absl/types",
        "abseil-cpp/absl/numeric",
    ];

    let dst = {
        let mut config = CmakeConfig::new("grpc");

        if get_env("CARGO_CFG_TARGET_OS").map_or(false, |s| s == "macos") {
            config.cxxflag("-stdlib=libc++");
        }

        // Ensure CoreFoundation be found in macos or ios
        if get_env("CARGO_CFG_TARGET_OS").map_or(false, |s| s == "macos")
            || get_env("CARGO_CFG_TARGET_OS").map_or(false, |s| s == "ios")
        {
            println!("cargo:rustc-link-lib=framework=CoreFoundation");
        }

        if let Some(val) = get_env("CXX") {
            config.define("CMAKE_CXX_COMPILER", val);
        } else if env::var("CARGO_CFG_TARGET_ENV").unwrap() == "musl" {
            config.define("CMAKE_CXX_COMPILER", "g++");
        }

        // Cross-compile support for iOS
        match env::var("TARGET").unwrap().as_str() {
            "aarch64-apple-ios" => {
                config
                    .define("CMAKE_OSX_SYSROOT", "iphoneos")
                    .define("CMAKE_OSX_ARCHITECTURES", "arm64");
            }
            "armv7-apple-ios" => {
                config
                    .define("CMAKE_OSX_SYSROOT", "iphoneos")
                    .define("CMAKE_OSX_ARCHITECTURES", "armv7");
            }
            "armv7s-apple-ios" => {
                config
                    .define("CMAKE_OSX_SYSROOT", "iphoneos")
                    .define("CMAKE_OSX_ARCHITECTURES", "armv7s");
            }
            "i386-apple-ios" => {
                config
                    .define("CMAKE_OSX_SYSROOT", "iphonesimulator")
                    .define("CMAKE_OSX_ARCHITECTURES", "i386");
            }
            "x86_64-apple-ios" => {
                config
                    .define("CMAKE_OSX_SYSROOT", "iphonesimulator")
                    .define("CMAKE_OSX_ARCHITECTURES", "x86_64");
            }
            _ => {}
        };

        // Allow overriding of the target passed to cmake
        // (needed for Android crosscompile)
        if let Ok(val) = env::var("CMAKE_TARGET_OVERRIDE") {
            config.target(&val);
        }

        // We don't need to generate install targets.
        config.define("gRPC_INSTALL", "false");
        // We don't need to build csharp target.
        config.define("gRPC_BUILD_CSHARP_EXT", "false");
        // We don't need to build codegen target.
        config.define("gRPC_BUILD_CODEGEN", "false");
        // We don't need to build benchmarks.
        config.define("gRPC_BENCHMARK_PROVIDER", "none");
        if cfg!(feature = "openssl") {
            config.define("gRPC_SSL_PROVIDER", "package");
            if cfg!(feature = "openssl-vendored") {
                config.register_dep("openssl");
            }
        } else if cfg!(feature = "secure") {
            third_party.extend_from_slice(&["boringssl-with-bazel"]);
        }
        if cfg!(feature = "no-omit-frame-pointer") {
            config
                .cflag("-fno-omit-frame-pointer")
                .cxxflag("-fno-omit-frame-pointer");
        }
        // Uses zlib from libz-sys.
        setup_libz(&mut config);
        config.build_target(library).uses_cxx11().build()
    };

    let build_dir = format!("{}/build", dst.display());
    if get_env("CARGO_CFG_TARGET_OS").map_or(false, |s| s == "windows") {
        let profile = match &*env::var("PROFILE").unwrap() {
            "bench" | "release" => "Release",
            _ => "Debug",
        };
        println!("cargo:rustc-link-search=native={}/{}", build_dir, profile);
        for path in third_party {
            println!(
                "cargo:rustc-link-search=native={}/third_party/{}/{}",
                build_dir, path, profile
            );
        }
    } else {
        println!("cargo:rustc-link-search=native={}", build_dir);
        for path in third_party {
            println!(
                "cargo:rustc-link-search=native={}/third_party/{}",
                build_dir, path,
            );
        }
    }

    // link libz
    println!("cargo:rustc-link-lib=static=z");
    // link cares
    println!("cargo:rustc-link-lib=static=cares");
    // link address_sorting
    println!("cargo:rustc-link-lib=static=address_sorting");
    // link absl/base
    println!("cargo:rustc-link-lib=static=absl_base");
    println!("cargo:rustc-link-lib=static=absl_raw_logging_internal");
    println!("cargo:rustc-link-lib=static=absl_dynamic_annotations");
    println!("cargo:rustc-link-lib=static=absl_throw_delegate");
    println!("cargo:rustc-link-lib=static=absl_log_severity");
    println!("cargo:rustc-link-lib=static=absl_spinlock_wait");
    // link absl/strings
    println!("cargo:rustc-link-lib=static=absl_strings");
    println!("cargo:rustc-link-lib=static=absl_strings_internal");
    println!("cargo:rustc-link-lib=static=absl_str_format_internal");
    // link absl/time
    println!("cargo:rustc-link-lib=static=absl_civil_time");
    println!("cargo:rustc-link-lib=static=absl_time_zone");
    println!("cargo:rustc-link-lib=static=absl_time");
    // link absl/types
    println!("cargo:rustc-link-lib=static=absl_bad_optional_access");
    // link absl/numeric
    println!("cargo:rustc-link-lib=static=absl_int128");
    // link grpc related lib
    println!("cargo:rustc-link-lib=static=gpr");
    println!("cargo:rustc-link-lib=static=upb");
    println!("cargo:rustc-link-lib=static={}", library);

    if cfg!(feature = "secure") {
        if cfg!(feature = "openssl") && !cfg!(feature = "openssl-vendored") {
            figure_ssl_path(&build_dir);
        } else {
            println!("cargo:rustc-link-lib=static=ssl");
            println!("cargo:rustc-link-lib=static=crypto");
        }
    }

    cc.include("grpc/include");
}

fn figure_ssl_path(build_dir: &str) {
    let path = format!("{}/CMakeCache.txt", build_dir);
    let f = BufReader::new(std::fs::File::open(&path).unwrap());
    let mut cnt = 0;
    for l in f.lines() {
        let l = l.unwrap();
        let t = trim_start(&l, "OPENSSL_CRYPTO_LIBRARY:FILEPATH=")
            .or_else(|| trim_start(&l, "OPENSSL_SSL_LIBRARY:FILEPATH="));
        if let Some(s) = t {
            let path = Path::new(s);
            println!(
                "cargo:rustc-link-search=native={}",
                path.parent().unwrap().display()
            );
            cnt += 1;
        }
    }
    if cnt != 2 {
        panic!(
            "CMake cache invalid, file {} contains {} ssl keys!",
            path, cnt
        );
    }
    println!("cargo:rustc-link-lib=ssl");
    println!("cargo:rustc-link-lib=crypto");
}

fn setup_libz(config: &mut CmakeConfig) {
    config.define("gRPC_ZLIB_PROVIDER", "package");
    config.register_dep("Z");
    // cmake script expect libz.a being under ${DEP_Z_ROOT}/lib, but libz-sys crate put it
    // under ${DEP_Z_ROOT}/build. Append the path to CMAKE_PREFIX_PATH to get around it.
    let zlib_root = env::var("DEP_Z_ROOT").unwrap();
    let prefix_path = if let Ok(prefix_path) = env::var("CMAKE_PREFIX_PATH") {
        format!("{};{}/build", prefix_path, zlib_root)
    } else {
        format!("{}/build", zlib_root)
    };
    // To avoid linking system library, set lib path explicitly.
    println!("cargo:rustc-link-search=native={}/build", zlib_root);
    println!("cargo:rustc-link-search=native={}/lib", zlib_root);
    env::set_var("CMAKE_PREFIX_PATH", prefix_path);
}

fn get_env(name: &str) -> Option<String> {
    println!("cargo:rerun-if-env-changed={}", name);
    match env::var(name) {
        Ok(s) => Some(s),
        Err(VarError::NotPresent) => None,
        Err(VarError::NotUnicode(s)) => {
            panic!("unrecognize env var of {}: {:?}", name, s.to_string_lossy());
        }
    }
}

// Generate the bindings to grpc C-core.
// Try to disable the generation of platform-related bindings.
fn bindgen_grpc(mut config: bindgen::Builder, file_path: &PathBuf) {
    // Search header files with API interface
    let mut headers = Vec::new();
    for result in WalkDir::new(Path::new("./grpc/include")) {
        let dent = result.expect("Error happened when search headers");
        if !dent.file_type().is_file() {
            continue;
        }
        let mut file = fs::File::open(dent.path()).expect("couldn't open headers");
        let mut buf = String::new();
        file.read_to_string(&mut buf)
            .expect("Coundn't read header content");
        if buf.contains("GRPCAPI") || buf.contains("GPRAPI") {
            headers.push(String::from(dent.path().to_str().unwrap()));
        }
    }

    // To control the order of bindings
    headers.sort();
    for path in headers {
        config = config.header(path);
    }

    let cfg = config
        .header("grpc_wrap.cc")
        .clang_arg("-xc++")
        .clang_arg("-I./grpc/include")
        .clang_arg("-std=c++11")
        .rustfmt_bindings(true)
        .impl_debug(true)
        .whitelist_function(r"\bgrpc_.*")
        .whitelist_function(r"\bgpr_.*")
        .whitelist_function(r"\bgrpcwrap_.*")
        .whitelist_var(r"\bGRPC_.*")
        .whitelist_type(r"\bgrpc_.*")
        .whitelist_type(r"\bgpr_.*")
        .whitelist_type(r"\bgrpcwrap_.*")
        .whitelist_type(r"\bcensus_context.*")
        .whitelist_type(r"\bverify_peer_options.*")
        .blacklist_type(r"(__)?pthread.*")
        .blacklist_function(r"\bgpr_mu_.*")
        .blacklist_function(r"\bgpr_cv_.*")
        .blacklist_function(r"\bgpr_once_.*")
        .blacklist_type(r"gpr_mu")
        .blacklist_type(r"gpr_cv")
        .blacklist_type(r"gpr_once")
        .constified_enum_module(r"grpc_status_code")
        .default_enum_style(bindgen::EnumVariation::Rust {
            non_exhaustive: false,
        });
    println!("running {}", cfg.command_line_flags().join(" "));
    cfg.generate()
        .expect("Unable to generate grpc bindings")
        .write_to_file(file_path)
        .expect("Couldn't write bindings!");
}

// Determine if need to update bindings. Supported platforms do not
// need to be updated by default unless the UPDATE_BIND is specified.
// Other platforms use bindgen to generate the bindings every time.
fn config_binding_path(config: bindgen::Builder) {
    let file_path: PathBuf;
    let target = env::var("TARGET").unwrap();
    match target.as_str() {
        "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu" => {
            // Cargo treats nonexistent files changed, so we only emit the rerun-if-changed
            // directive when we expect the target-specific pre-generated binding file to be
            // present.
            println!("cargo:rerun-if-changed=bindings/{}-bindings.rs", &target);

            file_path = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
                .join("bindings")
                .join(format!("{}-bindings.rs", &target));
            if env::var("UPDATE_BIND").map(|s| s == "1").unwrap_or(false) {
                bindgen_grpc(config, &file_path);
            }
        }
        _ => {
            file_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("grpc-bindings.rs");
            bindgen_grpc(config, &file_path);
        }
    };
    println!(
        "cargo:rustc-env=BINDING_PATH={}",
        file_path.to_str().unwrap()
    );
}

fn main() {
    println!("cargo:rerun-if-changed=grpc_wrap.cc");
    println!("cargo:rerun-if-changed=grpc");
    println!("cargo:rerun-if-env-changed=UPDATE_BIND");

    // create a builder to compile grpc_wrap.cc
    let mut cc = cc::Build::new();
    // create a config to generate binding file
    let mut bind_config = bindgen::Builder::default();

    let library = if cfg!(feature = "secure") {
        cc.define("GRPC_SYS_SECURE", None);
        bind_config = bind_config.clang_arg("-DGRPC_SYS_SECURE");
        "grpc"
    } else {
        "grpc_unsecure"
    };

    if get_env("CARGO_CFG_TARGET_OS").map_or(false, |s| s == "windows") {
        // At lease vista
        cc.define("_WIN32_WINNT", Some("0x600"));
        bind_config = bind_config.clang_arg("-D _WIN32_WINNT=0x600");
    }

    if get_env("GRPCIO_SYS_USE_PKG_CONFIG").map_or(false, |s| s == "1") {
        // Print cargo metadata.
        let lib_core = probe_library(library, true);
        for inc_path in lib_core.include_paths {
            cc.include(inc_path);
        }
    } else {
        build_grpc(&mut cc, library);
    }

    cc.cpp(true);
    if !cfg!(target_env = "msvc") {
        cc.flag("-std=c++11");
    }
    cc.file("grpc_wrap.cc");
    cc.warnings_into_errors(true);
    cc.compile("libgrpc_wrap.a");

    config_binding_path(bind_config);
}
