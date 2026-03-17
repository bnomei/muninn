use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const HELPER_SOURCE: &str = "src/apple_speech_transcriber.swift";
const HELPER_OUTPUT: &str = "apple_speech_transcriber";

fn main() {
    println!("cargo:rerun-if-changed={HELPER_SOURCE}");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR must be set"));
    let output_path = out_dir.join(HELPER_OUTPUT);
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if target_os != "macos" {
        write_stub_helper(&output_path, stub_message());
        return;
    }

    let sdk_path = command_output("xcrun", &["--show-sdk-path"])
        .expect("xcrun --show-sdk-path must succeed for macOS Apple Speech builds");
    let sdk_version = command_output("xcrun", &["--show-sdk-version"])
        .expect("xcrun --show-sdk-version must succeed for macOS Apple Speech builds");

    if !sdk_supports_apple_speech(&sdk_version) {
        println!(
            "cargo:warning=falling back to Apple Speech stub helper because SDK version {sdk_version} is below macOS 26"
        );
        write_stub_helper(&output_path, stub_message());
        return;
    }

    let swift_target = swift_target_for(env::var("TARGET").unwrap_or_default(), &sdk_version)
        .expect("TARGET must be an Apple macOS architecture for Apple Speech builds");

    let status = Command::new("swiftc")
        .arg("-parse-as-library")
        .arg("-O")
        .arg("-target")
        .arg(swift_target)
        .arg("-sdk")
        .arg(&sdk_path)
        .arg(HELPER_SOURCE)
        .arg("-o")
        .arg(&output_path)
        .status();

    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            panic!("swiftc failed while compiling Apple Speech helper with exit status {status}");
        }
        Err(error) => {
            panic!("failed to launch swiftc for Apple Speech helper compilation: {error}");
        }
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn sdk_supports_apple_speech(sdk_version: &str) -> bool {
    sdk_version
        .split('.')
        .next()
        .and_then(|value| value.parse::<u32>().ok())
        .is_some_and(|major| major >= 26)
}

fn swift_target_for(target: String, sdk_version: &str) -> Option<String> {
    let arch = match target.split('-').next()? {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        _ => return None,
    };

    Some(format!("{arch}-apple-macosx{sdk_version}"))
}

fn write_stub_helper(path: &Path, message: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create Apple Speech helper output directory");
    }

    let script = format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", stub_json(message));
    fs::write(path, script).expect("write Apple Speech stub helper");
}

fn stub_json(message: &str) -> String {
    let escaped = message.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "{{\"outcome\":\"unavailable_platform\",\"code\":\"unsupported_apple_speech_platform\",\"message\":\"{escaped}\",\"transcript\":null,\"resolved_locale\":null,\"asset_status\":null}}"
    )
}

fn stub_message() -> &'static str {
    "Apple Speech transcription requires a macOS 26+ build with the modern Speech framework helper"
}
