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
    for variable in [
        "RC",
        "PATH",
        "WindowsSdkVerBinPath",
        "WindowsSdkDir",
        "WindowsSDKVersion",
        "ProgramFiles(x86)",
        "CARGO_CFG_TARGET_ARCH",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo did not set OUT_DIR"))
        .join("notepad-classic.res");
    let compiler = find_resource_compiler().unwrap_or_else(|message| panic!("{message}"));
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

fn find_resource_compiler() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("RC") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH")
        .map_err(|_| "Cargo did not set CARGO_CFG_TARGET_ARCH".to_owned())?;
    let architecture = match target_arch.as_str() {
        "x86" => "x86",
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => {
            return Err(format!(
                "unsupported Windows target architecture {other:?}; set RC to a compatible resource compiler"
            ));
        }
    };

    if let Some(path) = find_on_path("rc.exe") {
        return Ok(path);
    }

    if let Some(root) = env::var_os("WindowsSdkVerBinPath") {
        let candidate = PathBuf::from(root).join(architecture).join("rc.exe");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    if let (Some(root), Ok(version)) = (env::var_os("WindowsSdkDir"), env::var("WindowsSDKVersion"))
    {
        let version = version.trim_end_matches(['\\', '/']);
        let candidate = PathBuf::from(root)
            .join("bin")
            .join(version)
            .join(architecture)
            .join("rc.exe");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    let kits_root = env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"))
        .join("Windows Kits\\10\\bin");
    let mut versions = fs::read_dir(&kits_root)
        .map_err(|error| {
            format!(
                "unable to search for the Windows resource compiler in {}: {error}; install the Windows SDK or set RC",
                kits_root.display()
            )
        })?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            sdk_version(&path).map(|version| (version, path))
        })
        .collect::<Vec<_>>();
    versions.sort_by(|left, right| right.0.cmp(&left.0));

    versions
        .into_iter()
        .map(|(_, version)| version.join(architecture).join("rc.exe"))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            format!(
                "unable to find rc.exe for Windows architecture {architecture}; install/configure the Windows SDK or set RC"
            )
        })
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}

fn sdk_version(path: &std::path::Path) -> Option<Vec<u32>> {
    if !path.is_dir() {
        return None;
    }
    path.file_name()?
        .to_str()?
        .split('.')
        .map(str::parse)
        .collect::<Result<Vec<_>, _>>()
        .ok()
}
