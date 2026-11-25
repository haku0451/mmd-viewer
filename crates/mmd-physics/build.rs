use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let manifest_dir = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).to_path_buf();
    let bullet = manifest_dir.join("../../vendor/bullet3");
    if !bullet.join("src/LinearMath/btVector3.cpp").is_file() {
        panic!(
            "vendor/bullet3 is missing. Run `git submodule update --init vendor/bullet3` and check out tag 3.25."
        );
    }

    let target = env::var("TARGET").unwrap_or_default();
    let mut build = cxx_build::bridge("src/bullet.rs");
    build
        .file("src/bullet_world.cpp")
        .include(bullet.join("src"))
        .std("c++17")
        .debug(false)
        .warnings(false)
        .define("NDEBUG", None)
        .define("NOMINMAX", None)
        .define("WIN32_LEAN_AND_MEAN", None);
    if target.contains("msvc") {
        build
            .flag("/O2")
            .flag("/EHsc")
            .flag("/bigobj")
            .flag("/wd4244")
            .flag("/wd4267")
            .flag("/wd4305")
            .flag("/wd4018")
            .flag("/wd4127");
    } else {
        build.flag("-O2").flag("-w");
    }
    for source in bullet_sources(&bullet, &target) {
        build.file(source);
    }
    build.compile("mmd_bullet");

    println!("cargo:rerun-if-changed=src/bullet.rs");
    println!("cargo:rerun-if-changed=src/bullet_world.cpp");
    println!("cargo:rerun-if-changed=src/bullet_world.h");
    println!("cargo:rerun-if-changed=build.rs");
}

fn bullet_sources(bullet: &Path, target: &str) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for relative in [
        "src/LinearMath",
        "src/BulletCollision",
        "src/BulletDynamics",
    ] {
        collect_cpp(&bullet.join(relative), target, &mut files);
    }
    files
}

fn collect_cpp(dir: &Path, target: &str, files: &mut Vec<std::path::PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|error| {
        panic!("failed to read {}: {error}", dir.display());
    });
    for entry in entries {
        let entry = entry.expect("bullet source entry");
        let path = entry.path();
        if path.is_dir() {
            collect_cpp(&path, target, files);
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".cpp") || name.ends_with("All.cpp") {
            continue;
        }
        if name == "btThreadSupportPosix.cpp" && target.contains("windows") {
            continue;
        }
        if name == "btThreadSupportWin32.cpp" && !target.contains("windows") {
            continue;
        }
        files.push(path);
    }
}
