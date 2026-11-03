fn main() {
    let rc = write_resources("assets/icon.ico", "Orchestrator", "orchestrator.exe");
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().unwrap();
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=ui");
}

/// Windows resources: the icon and version info (what Explorer, Task Manager and
/// the Start menu show), generated from the Cargo version.
fn write_resources(icon: &str, file_description: &str, original_filename: &str) -> std::path::PathBuf {
    let version = env!("CARGO_PKG_VERSION");
    let mut parts: Vec<u32> = version.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    parts.resize(4, 0);
    let numeric = parts.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    // Absolute, with forward slashes (fine in .rc files, and no escaping needed).
    let icon: String = std::fs::canonicalize(icon).unwrap().to_string_lossy().chars().map(|c| if c == std::path::MAIN_SEPARATOR { '/' } else { c }).collect();
    let icon = icon.trim_start_matches("//?/");
    let rc = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "therealfunnyfood"
      VALUE "FileDescription", "{file_description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{original_filename}"
      VALUE "LegalCopyright", "Copyright (c) 2026 therealfunnyfood. MIT License."
      VALUE "OriginalFilename", "{original_filename}"
      VALUE "ProductName", "Orchestrator"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("resources.rc");
    std::fs::write(&out, rc).unwrap();
    out
}
