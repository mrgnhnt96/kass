#[cfg(target_os = "macos")]
use std::process::Command;

fn main() {
    // Link Swift runtime libraries for screencapturekit crate
    #[cfg(target_os = "macos")]
    {
        // ScreenCaptureKit does not exist on macOS 11, so weak-link it to
        // allow the app to launch and gate usage at runtime instead.
        println!("cargo:rustc-link-arg=-Wl,-weak_framework,ScreenCaptureKit");

        // SMAppService, for launching at login (login_item.rs).
        println!("cargo:rustc-link-lib=framework=ServiceManagement");

        // Add Swift runtime library paths to RPATH
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
        println!("cargo:rustc-link-arg=-L/usr/lib/swift");

        // Also try Xcode's Swift libraries
        if let Ok(output) = Command::new("xcode-select").arg("-p").output() {
            if output.status.success() {
                let xcode_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let swift_lib_path = format!(
                    "{}/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx",
                    xcode_path
                );
                println!("cargo:rustc-link-arg=-Wl,-rpath,{}", swift_lib_path);
                println!("cargo:rustc-link-arg=-L{}", swift_lib_path);
            }
        }
    }

    let project_root = env!("CARGO_MANIFEST_DIR");
    let gen_dir = format!("{}/gen", project_root);
    std::fs::create_dir_all(&gen_dir).expect("Failed to create gen directory");

    // Compile macOS Liquid Glass icon
    #[cfg(target_os = "macos")]
    {
        // kass.icon is in tauri/assets/kass.icon (one level up from src-tauri)
        let icon_source = format!("{}/../assets/kass.icon", project_root);

        if std::path::Path::new(&icon_source).exists() {
            println!("cargo:rerun-if-changed={}", icon_source);
            println!("cargo:rerun-if-changed={}/icon.json", icon_source);
            println!("cargo:rerun-if-changed={}/Assets", icon_source);

            let partial_plist = format!("{}/partial.plist", gen_dir);
            let output = Command::new("xcrun")
                .args([
                    "actool",
                    "--compile",
                    &gen_dir,
                    "--output-format",
                    "human-readable-text",
                    "--output-partial-info-plist",
                    &partial_plist,
                    "--app-icon",
                    "kass",
                    "--include-all-app-icons",
                    "--target-device",
                    "mac",
                    "--minimum-deployment-target",
                    "11.0",
                    "--platform",
                    "macosx",
                    &icon_source,
                ])
                .output();

            match output {
                Ok(output) => {
                    if !output.status.success() {
                        eprintln!("actool stderr: {}", String::from_utf8_lossy(&output.stderr));
                        eprintln!("actool stdout: {}", String::from_utf8_lossy(&output.stdout));
                        panic!("actool failed to compile icon");
                    }
                    println!("Successfully compiled icon to {}", gen_dir);
                }
                Err(e) => {
                    eprintln!("Failed to execute xcrun actool: {}", e);
                    eprintln!("Make sure you have Xcode Command Line Tools installed");
                    panic!("Icon compilation failed");
                }
            }

            // Generate kass.icns from the source PNG via sips + iconutil
            let icns_path = format!("{}/kass.icns", gen_dir);
            if !std::path::Path::new(&icns_path).exists() {
                let source_png = format!("{}/Assets/Kass.png", icon_source);
                if std::path::Path::new(&source_png).exists() {
                    let iconset_dir = format!("{}/kass.iconset", gen_dir);
                    std::fs::create_dir_all(&iconset_dir).ok();

                    let sizes: &[(u32, &str)] = &[
                        (16, "icon_16x16.png"),
                        (32, "icon_16x16@2x.png"),
                        (32, "icon_32x32.png"),
                        (64, "icon_32x32@2x.png"),
                        (128, "icon_128x128.png"),
                        (256, "icon_128x128@2x.png"),
                        (256, "icon_256x256.png"),
                        (512, "icon_256x256@2x.png"),
                        (512, "icon_512x512.png"),
                        (1024, "icon_512x512@2x.png"),
                    ];

                    for (size, name) in sizes {
                        let dest = format!("{}/{}", iconset_dir, name);
                        let status = Command::new("sips")
                            .args([
                                "-z",
                                &size.to_string(),
                                &size.to_string(),
                                &source_png,
                                "--out",
                                &dest,
                            ])
                            .output();
                        if let Ok(out) = status {
                            if !out.status.success() {
                                eprintln!(
                                    "sips failed for {}: {}",
                                    name,
                                    String::from_utf8_lossy(&out.stderr)
                                );
                            }
                        }
                    }

                    let iconutil_output = Command::new("iconutil")
                        .args(["-c", "icns", "-o", &icns_path, &iconset_dir])
                        .output();

                    match iconutil_output {
                        Ok(out) if out.status.success() => {
                            println!("Generated kass.icns");
                        }
                        Ok(out) => {
                            eprintln!("iconutil failed: {}", String::from_utf8_lossy(&out.stderr));
                        }
                        Err(e) => {
                            eprintln!("Failed to run iconutil: {}", e);
                        }
                    }

                    // Clean up iconset directory
                    std::fs::remove_dir_all(&iconset_dir).ok();
                }
            }
        } else {
            println!(
                "cargo:warning=Icon source not found at {}, skipping icon compilation",
                icon_source
            );
        }
    }

    // Ensure all resource files exist so Tauri's bundler doesn't fail.
    // On non-macOS these are always stubs. On macOS, actool may not produce
    // Assets.car if the Xcode version doesn't support the .icon format.
    {
        let required = ["Assets.car", "kass.icns", "partial.plist"];
        for name in required {
            let path = format!("{}/{}", gen_dir, name);
            if !std::path::Path::new(&path).exists() {
                std::fs::write(&path, b"").ok();
            }
        }
    }

    #[cfg(target_os = "macos")]
    embed_dev_info_plist();
}

/// A development build runs as a bare binary with no bundle, so macOS reads
/// its Info.plist (the microphone and network usage strings, the bundle id)
/// from the binary itself. Bundled builds use the bundle's.
#[cfg(target_os = "macos")]
fn embed_dev_info_plist() {
    println!("cargo:rerun-if-changed=Info.plist");
    if std::env::var("PROFILE").as_deref() != Ok("debug") {
        return;
    }
    let plist = std::fs::read_to_string("Info.plist").expect("Info.plist");
    let extra = format!(
        "\t<key>CFBundleIdentifier</key>\n\t<string>com.mrgnhnt.kass</string>\n\
         \t<key>CFBundleName</key>\n\t<string>Kass</string>\n\
         \t<key>CFBundleShortVersionString</key>\n\t<string>{v}</string>\n\
         \t<key>CFBundleVersion</key>\n\t<string>{v}</string>\n</dict>",
        v = std::env::var("CARGO_PKG_VERSION").unwrap_or_default()
    );
    let merged = match plist.rfind("</dict>") {
        Some(end) => format!(
            "{}{}{}",
            &plist[..end],
            extra,
            &plist[end + "</dict>".len()..]
        ),
        None => plist,
    };
    let path = format!("{}/Info.plist", std::env::var("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(&path, merged).expect("write dev Info.plist");
    println!("cargo:rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{path}");
}
