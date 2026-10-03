use std::path::PathBuf;

use xmux::model::source::{ExecRunner, Runner};
use xmux::transport::{Login, Ssh, Transport};

#[ignore = "requires XMUX_LIVE_PW_HOST, PORT, USER, and PASS"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn password_login_later_command_and_key_registration_without_multiplexing() {
    let (Ok(host), Ok(port), Ok(user), Ok(password)) = (
        std::env::var("XMUX_LIVE_PW_HOST"),
        std::env::var("XMUX_LIVE_PW_PORT"),
        std::env::var("XMUX_LIVE_PW_USER"),
        std::env::var("XMUX_LIVE_PW_PASS"),
    ) else {
        return;
    };
    // The test input becomes a broker-only secret before any ssh child is spawned.
    unsafe {
        std::env::remove_var("XMUX_LIVE_PW_PASS");
    }
    let port: u16 = port.parse().expect("XMUX_LIVE_PW_PORT must be a port");
    let root = std::env::temp_dir().join(format!(
        "xmux-live-password-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(&root).expect("create live test directory");
    let key = root.join("id_ed25519");
    let key_status = std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .status()
        .expect("run ssh-keygen");
    assert!(key_status.success(), "ssh-keygen failed");
    let public_key =
        std::fs::read_to_string(key.with_extension("pub")).expect("read generated public key");

    let login = Login {
        address: Some(host.clone()),
        port: Some(port),
        user: Some(user.clone()),
    };
    let credentials = xmux::transport::auth::Credentials::new_with_helper(
        root.clone(),
        PathBuf::from(env!("CARGO_BIN_EXE_xmux")),
        true,
    );
    credentials
        .begin("xmux-live-password", login.clone(), password)
        .expect("start pending credential")
        .expect("nonempty password");
    let mut transport = Ssh {
        id: "xmux-live-password".into(),
        alias: "xmux-live-password".into(),
        control_path: String::new(),
        os: "windows".into(),
        login,
        credentials: credentials.clone(),
        shell: xmux::transport::vocab::RemoteShell::Posix,
    };

    let login_command = transport
        .login_argv(xmux::transport::vocab::MARKED_SHELL_PROBE)
        .expect("login command");
    assert!(!login_command.join(" ").contains("ControlMaster"));
    let (_running, done) = xmux::link::unlock::start_login(
        "xmux-live-password".into(),
        login_command,
        xmux::link::unlock::LOGIN_IDLE,
    );
    let conversation = done.await.expect("login verdict");
    assert_eq!(conversation.outcome, xmux::link::unlock::UnlockOutcome::Ok);

    let listing = transport
        .raw_shell_argv("printf xmux-listing-ok")
        .expect("later command");
    let output = ExecRunner
        .run_spec(&listing)
        .await
        .expect("later command uses held password");
    assert_eq!(String::from_utf8_lossy(&output), "xmux-listing-ok");

    let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
    let attach_command = transport.interactive_attach_argv(&[
        "sh".into(),
        "-c".into(),
        "printf xmux-long-lived-ok; sleep 2".into(),
    ]);
    let attachment =
        xmux::display::attachment::spawn_attachment(&attach_command, 80, 24, 1, events, &[])
            .expect("spawn password-authenticated long-lived attachment");
    drop(attach_command);
    // Any output would include ssh's own refusal, so the remote shell's marker on the
    // screen is the proof the held password authenticated this child.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let _ = tokio::time::timeout(std::time::Duration::from_millis(250), received.recv()).await;
        let area = ratatui::layout::Rect::new(0, 0, 80, 24);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        attachment
            .grid
            .lock()
            .expect("grid lock")
            .render_into(&mut buffer, area);
        let screen: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        if screen.contains("xmux-long-lived-ok") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "long-lived attachment never reached the remote shell: {}",
            screen.trim()
        );
    }
    attachment.teardown();

    let registration = xmux::provision::env::authorized_keys_command(public_key.trim())
        .expect("registration command");
    let registration = transport
        .raw_shell_argv(&registration)
        .expect("registration ssh");
    ExecRunner
        .run_spec(&registration)
        .await
        .expect("registration uses held password");

    credentials.remove("xmux-live-password");
    transport.set_credentials(credentials.clone());
    let verified = std::process::Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            &format!("HostName={host}"),
            "-o",
            &format!("Port={port}"),
            "-o",
            &format!("User={user}"),
            "-i",
        ])
        .arg(&key)
        .args(["--", "xmux-live-password", "printf xmux-key-ok"])
        .status()
        .expect("verify registered key");
    assert!(verified.success(), "registered key did not authenticate");

    credentials.shutdown();
    std::fs::remove_dir_all(&root).expect("remove live test directory");
}

fn unique_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos()
        .to_string()
}
