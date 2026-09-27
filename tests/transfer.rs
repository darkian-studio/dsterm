use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

fn dsterm() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dsterm"))
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn wait_for_port(port: u16) {
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("listener on {port} never came up");
}

#[test]
fn auto_receive_without_listen_is_usage_error() {
    let out = Command::new(dsterm())
        .args(["--auto-receive"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--auto-receive requires --listen-transfer"),
        "{err}"
    );
}

#[test]
fn sender_preflight_rejects_missing_source_before_connect() {
    let out = Command::new(dsterm())
        .args(["transfer", "/definitely/not/here-xyz", "127.0.0.1:9"])
        .output()
        .unwrap();
    assert_ne!(out.status.code(), Some(0));
    let all =
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    assert!(all.contains("no such file"), "{all}");
}

#[test]
fn unbracketed_ipv6_endpoint_is_rejected() {
    let dir = std::env::temp_dir().join(format!("dsterm-t-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("probe.txt");
    std::fs::write(&src, "hi").unwrap();
    let out = Command::new(dsterm())
        .args(["transfer", &src.to_string_lossy(), "::1:8770"])
        .output()
        .unwrap();
    assert_ne!(out.status.code(), Some(0));
    let all =
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout);
    assert!(all.contains("bracket"), "{all}");
}

#[test]
fn file_transfer_to_auto_receiver() {
    let base = std::env::temp_dir().join(format!("dsterm-xfer-{}", std::process::id()));
    let src_dir = base.join("src");
    let dest = base.join("dest");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(src_dir.join("hello.txt"), "hello transfer").unwrap();

    let port = free_port();
    let mut listener = Command::new(dsterm())
        .args([
            "--listen-transfer",
            "--auto-receive",
            "-p",
            &port.to_string(),
            "--dest",
            &dest.to_string_lossy(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_port(port);

    let send = Command::new(dsterm())
        .args([
            "transfer",
            &src_dir.join("hello.txt").to_string_lossy().to_string(),
            &format!("127.0.0.1:{port}"),
        ])
        .output()
        .unwrap();
    assert!(
        send.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&send.stdout),
        String::from_utf8_lossy(&send.stderr)
    );
    let got = dest.join("hello.txt");
    assert_eq!(std::fs::read_to_string(&got).unwrap(), "hello transfer");

    // Conflicting destination rejects by default without moving bytes.
    let again = Command::new(dsterm())
        .args([
            "transfer",
            &src_dir.join("hello.txt").to_string_lossy().to_string(),
            &format!("127.0.0.1:{port}"),
        ])
        .output()
        .unwrap();
    assert!(!again.status.success());
    let all = String::from_utf8_lossy(&again.stderr).into_owned()
        + &String::from_utf8_lossy(&again.stdout);
    assert!(
        all.contains("already exists") || all.contains("destination"),
        "{all}"
    );

    listener.kill().ok();
    let _ = listener.wait();
}

#[test]
fn directory_with_symlink_and_dangling_link_roundtrips() {
    let base = std::env::temp_dir().join(format!("dsterm-dir-{}", std::process::id()));
    let src = base.join("tree");
    let dest = base.join("out");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a.txt"), "a").unwrap();
    std::fs::write(src.join("sub").join("b.txt"), "b").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("a.txt", src.join("link.txt")).unwrap();
        std::os::unix::fs::symlink("nope-target", src.join("dangling.txt")).unwrap();
    }

    let port = free_port();
    let mut listener = Command::new(dsterm())
        .args([
            "--listen-transfer",
            "--auto-receive",
            "-p",
            &port.to_string(),
            "--dest",
            &dest.to_string_lossy(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_port(port);

    let send = Command::new(dsterm())
        .args([
            "transfer",
            &src.to_string_lossy().to_string(),
            &format!("127.0.0.1:{port}"),
        ])
        .output()
        .unwrap();
    assert!(
        send.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&send.stdout),
        String::from_utf8_lossy(&send.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("tree").join("a.txt")).unwrap(),
        "a"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("tree").join("sub").join("b.txt")).unwrap(),
        "b"
    );
    #[cfg(unix)]
    {
        assert!(
            std::fs::symlink_metadata(dest.join("tree").join("dangling.txt"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    // No temp residue left beside the finalized tree.
    let _ = std::io::stderr().flush();
    for e in std::fs::read_dir(&dest).unwrap() {
        let n = e.unwrap().file_name().to_string_lossy().into_owned();
        assert!(!n.contains("dsterm-transfer"), "temp residue: {n}");
    }

    listener.kill().ok();
    let _ = listener.wait();
}
