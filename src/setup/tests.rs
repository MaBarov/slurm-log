use super::*;

#[test]
fn folder_browser_names_cannot_inject_terminal_controls() {
    assert_eq!(
        safe_terminal_name(std::ffi::OsStr::new("bad\u{1b}[2J\nname")),
        "bad�[2J�name"
    );
}

#[test]
fn ssh_config_parser_keeps_only_selectable_literal_aliases() {
    let aliases = ssh_aliases_from_text(
        "Host *\nHost cispa sprint *.internal !blocked\nHOST gpu-box # comment\n",
    );
    assert_eq!(aliases, vec!["cispa", "sprint", "gpu-box"]);
    assert!(wildcard_match("*.conf", "cluster.conf"));
    assert!(!wildcard_match("*.conf", "cluster.txt"));
}

#[test]
fn ssh_probe_values_become_safe_editable_defaults() {
    let probe = parse_ssh_probe(
        "SLURM_LOG_USER=alice\nSLURM_LOG_HOME=/remote/home/alice\nSLURM_LOG_CLUSTER=gpu lab\nSLURM_LOG_ACCOUNTING=yes\n",
    );
    assert_eq!(probe.user.as_deref(), Some("alice"));
    assert_eq!(probe.home.as_deref(), Some("/remote/home/alice"));
    assert_eq!(probe.accounting, Some(true));
    assert_eq!(safe_cluster_name("gpu lab", "remote"), "gpu-lab");
    assert_eq!(safe_cluster_name("all", "remote-alias"), "remote-alias");
}

#[test]
fn setup_parsers_cover_defaults_fallbacks_and_invalid_values() {
    assert_eq!(parse_selection("", 2).unwrap(), vec![0, 1]);
    assert!(parse_selection("none", 2).unwrap().is_empty());
    assert_eq!(parse_selection("2,1-2", 3).unwrap(), vec![0, 1]);
    assert!(parse_selection("word", 3).is_err());
    assert!(parse_selection("3-2", 3).is_err());

    for invalid in ["", "-host", "two hosts", "*", "host?", "!host"] {
        assert!(!literal_ssh_alias(invalid), "accepted {invalid:?}");
    }
    assert!(literal_ssh_alias("gpu-cluster_1"));
    assert!(wildcard_match("a?c", "abc"));
    assert!(wildcard_match("a*d", "abcd"));
    assert!(wildcard_match("*", "anything"));
    assert!(!wildcard_match("a?", "a"));
    assert!(!wildcard_match("ab*d", "abce"));

    assert_eq!(safe_cluster_name("---", "fallback"), "fallback");
    assert_eq!(safe_cluster_name("all", "both"), "cluster");
    assert_eq!(safe_cluster_name("gpu/name", "unused"), "gpu-name");
    assert_eq!(safe_cluster_name(&"x".repeat(80), "unused").len(), 48);

    let probe =
        parse_ssh_probe("noise\nSLURM_LOG_CLUSTER=\nSLURM_LOG_ACCOUNTING=no\nUNKNOWN=value\n");
    assert_eq!(probe.cluster, None);
    assert_eq!(probe.accounting, Some(false));
}

#[test]
fn include_globs_are_relative_sorted_and_file_only() {
    let temporary = tempfile::tempdir().unwrap();
    let ssh = temporary.path().join("ssh");
    fs::create_dir_all(ssh.join("parts/directory.conf")).unwrap();
    fs::write(ssh.join("config"), "").unwrap();
    fs::write(ssh.join("parts/b.conf"), "").unwrap();
    fs::write(ssh.join("parts/a.conf"), "").unwrap();

    assert_eq!(
        include_paths(&ssh.join("config"), "parts/*.conf"),
        vec![ssh.join("parts/a.conf"), ssh.join("parts/b.conf")]
    );
    assert_eq!(
        include_paths(&ssh.join("config"), "parts/a.conf"),
        vec![ssh.join("parts/a.conf")]
    );
    assert!(include_paths(&ssh.join("config"), "parts/missing").is_empty());
    assert!(include_paths(Path::new("config"), "*").is_empty());
}

#[test]
fn folder_children_are_sorted_and_exclude_files_and_symlinks() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    fs::create_dir(temporary.path().join("zeta")).unwrap();
    fs::create_dir(temporary.path().join("alpha")).unwrap();
    fs::write(temporary.path().join("file"), "").unwrap();
    symlink(
        temporary.path().join("alpha"),
        temporary.path().join("linked"),
    )
    .unwrap();
    assert_eq!(
        directory_children(temporary.path()),
        vec![
            temporary.path().join("alpha"),
            temporary.path().join("zeta")
        ]
    );
    assert!(directory_children(&temporary.path().join("missing")).is_empty());
    assert_eq!(browse_bank_directory(&[]).unwrap(), None);
}

#[test]
fn ssh_picker_keys_cover_navigation_selection_and_cancel() {
    let mut focus = 1;
    assert_eq!(
        apply_picker_key(KeyCode::Up, &mut focus, 3),
        PickerKey::Continue
    );
    assert_eq!(focus, 0);
    apply_picker_key(KeyCode::Down, &mut focus, 3);
    apply_picker_key(KeyCode::Char('j'), &mut focus, 3);
    apply_picker_key(KeyCode::Down, &mut focus, 3);
    assert_eq!(focus, 2);
    apply_picker_key(KeyCode::Home, &mut focus, 3);
    assert_eq!(focus, 0);
    apply_picker_key(KeyCode::End, &mut focus, 3);
    assert_eq!(focus, 2);
    assert_eq!(
        apply_picker_key(KeyCode::Enter, &mut focus, 3),
        PickerKey::Select
    );
    assert_eq!(
        apply_picker_key(KeyCode::Esc, &mut focus, 3),
        PickerKey::Cancel
    );
    assert_eq!(
        apply_picker_key(KeyCode::Char('x'), &mut focus, 3),
        PickerKey::Continue
    );
}

#[test]
fn folder_picker_keys_cover_navigation_parent_activation_and_cancel() {
    let mut focus = 1;
    let mut current = Some(PathBuf::from("/one/two"));
    apply_browser_key(KeyCode::Up, &mut current, &mut focus, 4);
    assert_eq!(focus, 0);
    apply_browser_key(KeyCode::Down, &mut current, &mut focus, 4);
    apply_browser_key(KeyCode::End, &mut current, &mut focus, 4);
    assert_eq!(focus, 3);
    apply_browser_key(KeyCode::Home, &mut current, &mut focus, 4);
    assert_eq!(focus, 0);
    apply_browser_key(KeyCode::Backspace, &mut current, &mut focus, 4);
    assert_eq!(current, Some(PathBuf::from("/one")));
    assert_eq!(
        apply_browser_key(KeyCode::Right, &mut current, &mut focus, 4),
        BrowserKey::Activate
    );
    assert_eq!(
        apply_browser_key(KeyCode::Char('q'), &mut current, &mut focus, 4),
        BrowserKey::Cancel
    );
}
