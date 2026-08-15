use super::*;
use std::{fs, os::unix::fs::symlink};

fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    fs::create_dir_all(root.join("runs/42")).unwrap();
    fs::write(root.join("runs/42/cpu_gate.json"), b"{\"passed\":true}").unwrap();
    fs::write(
        root.join("runs/42/verifier-receipt.txt"),
        b"digest: abc\ngate: ok\n",
    )
    .unwrap();
    fs::write(
        root.join("runs/42/weights.bin"),
        [0u8, 159, 146, 150, 1, 2, 3],
    )
    .unwrap();
    fs::write(root.join("runs/42/model.safetensors"), b"payload").unwrap();
    fs::write(root.join("unrelated.log"), b"noise").unwrap();
    directory
}

#[test]
fn glob_matches_basename_patterns_literally() {
    assert!(glob_match("cpu_gate.json", "cpu_gate.json"));
    assert!(glob_match("*.json", "cpu_gate.json"));
    assert!(glob_match("cpu_gate.*", "cpu_gate.json"));
    assert!(glob_match("verifier-???????.txt", "verifier-receipt.txt"));
    assert!(!glob_match("verifier-????.txt", "verifier-receipt.txt"));
    assert!(glob_match("weights.*", "weights.bin"));
    assert!(glob_match("*", "anything"));
    assert!(!glob_match("", "anything"));
    assert!(glob_match("?.json", "a.json"));
}

#[test]
fn mime_sniffing_distinguishes_common_formats() {
    assert_eq!(mime_sniff(b"{ }"), "application/json");
    assert_eq!(mime_sniff(b"  [1,2]"), "application/json");
    assert_eq!(mime_sniff(b"hello\nworld"), "text/plain");
    assert_eq!(
        mime_sniff(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "image/png"
    );
    assert_eq!(mime_sniff(&[0x1F, 0x8B, 0x08]), "application/gzip");
    assert_eq!(mime_sniff(b"PK\x03\x04rest"), "application/zip");
    assert_eq!(mime_sniff(b"%PDF-1.7"), "application/pdf");
    assert_eq!(
        mime_sniff(&[0x7F, b'E', b'L', b'F']),
        "application/x-executable"
    );
    assert_eq!(mime_sniff(&[0, 1, 2, 255]), "application/octet-stream");
    assert_eq!(mime_sniff(b"text\0binary"), "application/octet-stream");
}

#[test]
fn search_returns_bounded_mime_aware_matches() {
    let directory = fixture();
    let root = directory.path().join("root");
    let result = search(&root, Path::new("."), "*.json", 1024).unwrap();
    assert_eq!(result.matches.len(), 1);
    let value = &result.matches[0];
    assert_eq!(value["path"], "runs/42/cpu_gate.json");
    assert_eq!(value["mime_type"], "application/json");
    assert_eq!(value["content"], "{\"passed\":true}");
    assert_eq!(value["sha256"], sha256_hex(b"{\"passed\":true}"));
    assert!(
        result
            .matches
            .iter()
            .all(|m| m["name"].as_str().unwrap().ends_with(".json"))
    );

    let receipts = search(&root, Path::new("."), "verifier-*.txt", 1024).unwrap();
    assert_eq!(receipts.matches.len(), 1);
    assert_eq!(receipts.matches[0]["mime_type"], "text/plain");
    assert!(
        receipts.matches[0]["content"]
            .as_str()
            .unwrap()
            .contains("digest: abc")
    );

    let binaries = search(&root, Path::new("."), "*.bin", 1024).unwrap();
    assert_eq!(binaries.matches.len(), 1);
    assert_eq!(binaries.matches[0]["mime_type"], "application/octet-stream");
    assert_eq!(binaries.matches[0]["content"], Value::Null);
    assert_eq!(binaries.matches[0]["content_omitted"], "binary");
}

#[test]
fn search_is_confined_beneath_the_root_and_search_subdirectory() {
    let directory = fixture();
    let root = directory.path().join("root");
    let confined = search(&root, Path::new("runs"), "*.json", 1024).unwrap();
    assert_eq!(confined.matches.len(), 1);
    assert_eq!(confined.matches[0]["path"], "42/cpu_gate.json");

    symlink(directory.path(), root.join("escape")).unwrap();
    assert!(search(&root, Path::new("escape"), "*", 1024).is_err());

    fs::hard_link(
        directory.path().join("root/runs/42/weights.bin"),
        root.join("hard.bin"),
    )
    .unwrap();
    let hard = search(&root, Path::new("."), "hard.bin", 1024).unwrap();
    assert!(hard.matches.is_empty());
}

#[test]
fn search_marks_oversized_files_and_truncates_content() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root");
    fs::create_dir_all(root.join("sub")).unwrap();
    let big = b"x".repeat(4096);
    fs::write(root.join("sub/big.txt"), &big).unwrap();
    let result = search(&root, Path::new("."), "big.txt", 256).unwrap();
    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0]["content_truncated"], Value::Bool(true));
    assert_eq!(result.matches[0]["content"].as_str().unwrap().len(), 256);

    fs::write(root.join("sub/empty.txt"), b"").unwrap();
    let empty = search(&root, Path::new("."), "empty.txt", 256).unwrap();
    assert_eq!(empty.matches[0]["content"], "");
}

#[test]
fn search_root_rejects_escapes_and_absolute_paths() {
    assert_eq!(validate_search_root(".").unwrap(), PathBuf::from("."));
    assert_eq!(
        validate_search_root("runs/42").unwrap(),
        PathBuf::from("runs/42")
    );
    for hostile in ["..", "../x", "/etc", "a/../../b", ""] {
        if hostile.is_empty() {
            assert_eq!(validate_search_root(hostile).unwrap(), PathBuf::from("."));
        } else {
            assert!(validate_search_root(hostile).is_err(), "accepted {hostile}");
        }
    }
}

#[test]
fn text_content_advances_past_a_truncated_multibyte_character() {
    let (text, truncated) = text_content("text/plain", "héllo".as_bytes(), 2).unwrap();
    assert_eq!(text, "hé");
    assert!(truncated);
}
