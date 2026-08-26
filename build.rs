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

    if let Some(path) = find_on_path("rc.exe") {
        return Ok(path);
    }

    let host_arch = match env::consts::ARCH {
        "x86" => "x86",
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => "x64",
    };

    let candidate_architectures: &[&str] = match host_arch {
        "x64" => &["x64", "x86"],
        "arm64" => &["arm64", "x64", "x86"],
        "x86" => &["x86"],
        _ => &["x64", "x86"],
    };

    if let Some(root) = env::var_os("WindowsSdkVerBinPath") {
        for &arch in candidate_architectures {
            let candidate = PathBuf::from(&root).join(arch).join("rc.exe");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    if let (Some(root), Ok(version)) = (env::var_os("WindowsSdkDir"), env::var("WindowsSDKVersion"))
    {
        let version = version.trim_end_matches(['\\', '/']);
        for &arch in candidate_architectures {
            let candidate = PathBuf::from(&root)
                .join("bin")
                .join(version)
                .join(arch)
                .join("rc.exe");
            if candidate.is_file() {
                return Ok(candidate);
            }
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

    for (_, version) in versions {
        for &arch in candidate_architectures {
            let candidate = version.join(arch).join("rc.exe");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(
        "unable to find a runnable rc.exe in the Windows SDK; install the Windows SDK or set RC"
            .to_owned(),
    )
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
