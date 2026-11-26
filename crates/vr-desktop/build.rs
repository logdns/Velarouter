//! Embeds the application icon and version info into the Windows executable.
//!
//! `winres` only works when the *host* is Windows, so cross builds from macOS or
//! Linux silently produced an .exe with no icon (a generic blank window icon).
//! Driving `windres` ourselves works for both native and cross builds.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/brand/app-icons/windows/velarouter.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let prefix = if arch == "x86_64" {
        "x86_64-w64-mingw32"
    } else if arch == "aarch64" {
        "aarch64-w64-mingw32"
    } else {
        "i686-w64-mingw32"
    };
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| format!("{prefix}-windres"));
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let icon = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/brand/app-icons/windows/velarouter.ico");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".into());
    let numeric = version
        .split('.')
        .map(|p| p.parse::<u16>().unwrap_or(0))
        .chain([0, 0])
        .take(4)
        .collect::<Vec<_>>();
    let rc = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {a},{b},{c},{d}
PRODUCTVERSION {a},{b},{c},{d}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "Velarouter remote desktop"
      VALUE "ProductName", "Velarouter"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "OriginalFilename", "Velarouter.exe"
      VALUE "LegalCopyright", "Apache-2.0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = icon.display(),
        a = numeric[0],
        b = numeric[1],
        c = numeric[2],
        d = numeric[3],
        version = version,
    );
    let rc_path = out_dir.join("velarouter.rc");
    std::fs::write(&rc_path, rc).expect("write .rc");
    let obj = out_dir.join("velarouter-res.o");
    let status = Command::new(&windres)
        .arg("--input")
        .arg(&rc_path)
        .arg("--output")
        .arg(&obj)
        .arg("--output-format=coff")
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("cargo:rustc-link-arg={}", obj.display());
            println!("cargo:warning=embedded Windows icon and version info");
        }
        other => println!("cargo:warning=windres failed ({other:?}); the .exe will have no icon"),
    }
}
