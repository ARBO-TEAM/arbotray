//! Embeds the application icon into the `.exe`.
//!
//! `rc.exe` from the Windows SDK is called directly rather than adding a
//! resource-compiling crate: this is the app's only resource and the whole job
//! is a few linker arguments. A machine without the SDK is not an error — the
//! build still succeeds, the `.exe` simply carries no icon and the tray falls
//! back to a stock one.

use std::path::PathBuf;
use std::process::Command;

/// Resource id the icon is stored under. `icon.rs` loads it back by this
/// number, so the two have to agree.
const ICON_ID: u16 = 1;

const ICON: &str = "assets/arbotray.ico";

fn main() {
    println!("cargo:rerun-if-changed={ICON}");
    println!("cargo:rerun-if-changed=build.rs");

    if let Err(e) = embed() {
        println!("cargo:warning=arbotray: icon not embedded ({e}); building without one");
    }
}

fn embed() -> Result<(), String> {
    // Only meaningful for Windows targets; a cross-compile to anything else
    // has no business looking for the Windows SDK.
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.contains("windows") {
        return Ok(());
    }

    let icon = PathBuf::from(ICON);
    if !icon.is_file() {
        return Err(format!("{ICON} is missing"));
    }
    let icon = icon
        .canonicalize()
        .map_err(|e| format!("resolve {ICON}: {e}"))?;

    let rc = find_rc(&target).ok_or("no rc.exe in the Windows SDK and no $RC")?;

    let out = PathBuf::from(std::env::var("OUT_DIR").map_err(|e| e.to_string())?)
        .join("arbotray.res");
    let script = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("arbotray.rc");

    // An .rc file treats a backslash as an escape, so the path goes in with
    // them doubled. Forward slashes would also work but are not what a
    // Windows-native tool round-trips cleanly.
    std::fs::write(
        &script,
        format!(
            "{} ICON \"{}\"\n",
            ICON_ID,
            icon.display().to_string().replace('\\', "\\\\")
        ),
    )
    .map_err(|e| format!("write {}: {e}", script.display()))?;

    let result = Command::new(&rc)
        .arg("/nologo")
        .arg(format!("/fo{}", out.display()))
        .arg(&script)
        .output()
        .map_err(|e| format!("run {}: {e}", rc.display()))?;

    if !result.status.success() {
        return Err(format!(
            "rc.exe failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }

    // `link.exe` takes a .res straight as an input file.
    println!("cargo:rustc-link-arg={}", out.display());
    Ok(())
}

/// Locate `rc.exe`.
///
/// The SDK keeps one under `Windows Kits\10\bin\<version>\<arch>\`; we want the
/// newest version and the architecture matching the target, since the resource
/// is linked into that target's image.
fn find_rc(target: &str) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("RC") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }

    let arch = if target.starts_with("aarch64") {
        "arm64"
    } else if target.starts_with("i686") {
        "x86"
    } else {
        "x64"
    };

    let kits = PathBuf::from(std::env::var_os("ProgramFiles(x86)")?)
        .join("Windows Kits")
        .join("10")
        .join("bin");

    // Version directories sort lexicographically in SDK order (10.0.22621.0),
    // so the last one is the newest.
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&kits)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|dir| dir.is_dir())
        .collect();
    versions.sort();

    versions
        .iter()
        .rev()
        .map(|version| version.join(arch).join("rc.exe"))
        .find(|candidate| candidate.is_file())
        .or_else(|| newest_rc_anywhere(&versions))
}

/// Last resort: an SDK installed for a different host architecture still
/// produces a resource the linker can use.
fn newest_rc_anywhere(versions: &[PathBuf]) -> Option<PathBuf> {
    for version in versions.iter().rev() {
        for arch in ["x64", "x86", "arm64"] {
            let candidate = version.join(arch).join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
