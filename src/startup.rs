//! `dsterm startup` — install an OS-native supervisor entry so the relay host
//! starts on boot. Uses a systemd user unit on Linux and a Termux:Boot script
//! on Android/Termux. PM2 is intentionally NOT used.
use std::path::PathBuf;

fn home_dir() -> anyhow::Result<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        Ok(PathBuf::from(home))
    } else {
        Ok(std::env::current_dir()?)
    }
}

fn is_termux() -> bool {
    std::env::var("TERMUX_VERSION").is_ok()
        || std::path::Path::new("/data/data/com.termux").exists()
}

fn exe_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "dsterm".to_string())
}

pub fn systemd_unit(exe: &str) -> String {
    let mut unit = String::new();
    unit.push_str("[Unit]\n");
    unit.push_str("Description=DSTerm relay host\n");
    unit.push_str("After=network-online.target\n");
    unit.push_str("Wants=network-online.target\n\n");
    unit.push_str("[Service]\n");
    unit.push_str(&format!("ExecStart={exe} host\n"));
    unit.push_str("Restart=on-failure\n");
    unit.push_str("RestartSec=5\n\n");
    unit.push_str("[Install]\n");
    unit.push_str("WantedBy=default.target\n");
    unit
}

pub fn termux_boot_script(exe: &str) -> String {
    format!("#!/data/data/com.termux/files/usr/bin/sh\n{exe} host &\n")
}

pub fn install() -> anyhow::Result<String> {
    let home = home_dir()?;
    let exe = exe_path();

    if is_termux() {
        let dir = home.join(".termux").join("boot");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("dsterm-host");
        std::fs::write(&path, termux_boot_script(&exe))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)?.permissions();
            perms.set_mode(0o700);
            std::fs::set_permissions(&path, perms)?;
        }
        Ok(format!(
            "Installed Termux:Boot script at {}. Install the Termux:Boot app and reboot to enable autostart.",
            path.display()
        ))
    } else {
        let dir = home.join(".config").join("systemd").join("user");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("dsterm.service");
        std::fs::write(&path, systemd_unit(&exe))?;
        Ok(format!(
            "Installed systemd user unit at {}. Enable it with: systemctl --user enable --now dsterm.service",
            path.display()
        ))
    }
}
