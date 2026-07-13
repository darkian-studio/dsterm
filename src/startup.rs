//! `dsterm startup` — install an OS-native supervisor entry so the relay host
//! starts on boot. Uses a Termux:Boot script on Android/Termux, a systemd user
//! unit on Linux, a launchd LaunchAgent on macOS, and a per-user Startup-folder
//! script on Windows. PM2 is intentionally NOT used.
use std::path::PathBuf;

pub fn home_dir() -> anyhow::Result<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        Ok(PathBuf::from(home))
    } else {
        Ok(std::env::current_dir()?)
    }
}

fn is_termux() -> bool {
    // Rely on the env var Termux always sets. A `/data/data/com.termux` path
    // probe false-positives as `C:\data\data\com.termux` on Windows.
    std::env::var("TERMUX_VERSION").is_ok()
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
    unit.push_str(&format!("ExecStart={exe} host --remote\n"));
    unit.push_str("Restart=on-failure\n");
    unit.push_str("RestartSec=5\n\n");
    unit.push_str("[Install]\n");
    unit.push_str("WantedBy=default.target\n");
    unit
}

pub fn termux_boot_script(exe: &str) -> String {
    format!("#!/data/data/com.termux/files/usr/bin/sh\n{exe} host --remote &\n")
}

pub fn launchd_plist(exe: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
    <key>Label</key>\n\
    <string>io.darkian.dsterm</string>\n\
    <key>ProgramArguments</key>\n\
    <array>\n\
        <string>{exe}</string>\n\
        <string>host</string>\n\
        <string>--remote</string>\n\
    </array>\n\
    <key>RunAtLoad</key>\n\
    <true/>\n\
    <key>KeepAlive</key>\n\
    <true/>\n\
</dict>\n\
</plist>\n"
    )
}

pub fn windows_startup_script(exe: &str) -> String {
    format!("@echo off\r\nstart \"\" \"{exe}\" host --remote\r\n")
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
        return Ok(format!(
            "Installed Termux:Boot script at {}. Install the Termux:Boot app and reboot to enable autostart.",
            path.display()
        ));
    }

    match std::env::consts::OS {
        "macos" => {
            let dir = home.join("Library").join("LaunchAgents");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("io.darkian.dsterm.plist");
            std::fs::write(&path, launchd_plist(&exe))?;
            Ok(format!(
                "Installed launchd agent at {}. Enable it with: launchctl load {}",
                path.display(),
                path.display()
            ))
        }
        "windows" => {
            let startup = std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("AppData").join("Roaming"))
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join("Startup");
            std::fs::create_dir_all(&startup)?;
            let path = startup.join("dsterm-host.bat");
            std::fs::write(&path, windows_startup_script(&exe))?;
            Ok(format!(
                "Installed Startup script at {}. It runs automatically on your next sign-in.",
                path.display()
            ))
        }
        _ => {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_unit_runs_host_remote() {
        let unit = systemd_unit("/usr/bin/dsterm");
        assert!(unit.contains("ExecStart=/usr/bin/dsterm host --remote\n"));
    }

    #[test]
    fn termux_boot_script_runs_host_remote() {
        let script = termux_boot_script("/usr/bin/dsterm");
        assert!(script.contains("/usr/bin/dsterm host --remote &"));
    }

    #[test]
    fn launchd_plist_runs_host_remote() {
        let plist = launchd_plist("/opt/dsterm");
        assert!(plist.contains("<string>/opt/dsterm</string>"));
        assert!(plist.contains("<string>host</string>"));
        assert!(plist.contains("<string>--remote</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
    }

    #[test]
    fn windows_startup_script_runs_host_remote() {
        let script = windows_startup_script("C:\\dsterm.exe");
        assert!(script.contains("start \"\" \"C:\\dsterm.exe\" host --remote"));
        assert!(script.contains("\r\n"));
    }
}
