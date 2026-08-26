use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    if env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let assets = PathBuf::from("assets");
    println!("cargo:rerun-if-changed=assets/notepad-classic.ico");
    println!("cargo:rerun-if-changed=assets/notepad-classic.rc");

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo did not set OUT_DIR"))
        .join("notepad-classic.res");
    let compiler = find_resource_compiler();
    let status = Command::new(&compiler)
        .current_dir(&assets)
        .args(["/nologo", "/fo"])
        .arg(&output)
        .arg("notepad-classic.rc")
        .status()
        .unwrap_or_else(|error| panic!("unable to run {}: {error}", compiler.display()));
    assert!(status.success(), "Windows resource compiler failed");
    println!("cargo:rustc-link-arg={}", output.display());
}

fn find_resource_compiler() -> PathBuf {
    if let Some(path) = env::var_os("RC") {
        return PathBuf::from(path);
    }

    let architecture = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86") => "x86",
        Ok("aarch64") => "arm64",
        _ => "x64",
    };
    let kits_root = env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"))
        .join("Windows Kits\\10\\bin");
    let mut versions = fs::read_dir(&kits_root)
        .unwrap_or_else(|error| panic!("unable to search {}: {error}", kits_root.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    versions.sort();
    versions.reverse();

    versions
        .into_iter()
        .map(|version| version.join(architecture).join("rc.exe"))
        .find(|path| path.is_file())
        .unwrap_or_else(|| {
            panic!(
                "unable to find rc.exe for {architecture}; install the Windows SDK or set the RC environment variable"
            )
        })
}
