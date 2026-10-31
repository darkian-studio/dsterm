//! `dsterm startup` — install an OS-native supervisor entry
//! so the relay host starts on boot. Uses a Termux:Boot
//! script on Android/Termux, a systemd user unit on Linux, a //! launchd LaunchAgent on macOS, and a per-user Startup-folder
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
    // Rely on the env var Termux always sets. A
    // `/data/data/com.termux` path probe false-positives as
    // `C:\data\data\com.termux` on Windows.
    std::env::var("TERMUX_VERSION").is_ok()
}

fn exe_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "dsterm".to_string())
}

pub fn systemd_unit(exe: &str, with_filesystem: bool) -> String {
    let mut unit = String::new();
    unit.push_str("[Unit]\n");
    unit.push_str("Description=DSTerm relay host\n");
    unit.push_str("After=network-online.target\n");
    unit.push_str("Wants=network-online.target\n\n");
    unit.push_str("[Service]\n");
    unit.push_str(&format!(
        "ExecStart={exe} host{}\n",
        remote_args(with_filesystem)
    ));
    unit.push_str("Restart=on-failure\n");
    unit.push_str("RestartSec=5\n\n");
    unit.push_str("[Install]\n");
    unit.push_str("WantedBy=default.target\n");
    unit
}

pub fn termux_boot_script(exe: &str, with_filesystem: bool) -> String {
    format!(
        "#!/data/data/com.termux/files/usr/bin/sh\n{exe} host{} &\n",
        remote_args(with_filesystem)
    )
}

pub fn launchd_plist(exe: &str, with_filesystem: bool) -> String {
    let remote_arg = if with_filesystem {
        "        <string>--remote</string>\n    "
    } else {
        "    "
    };
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
{remote_arg}</array>\n\
    <key>RunAtLoad</key>\n\
    <true/>\n\
    <key>KeepAlive</key>\n\
    <true/>\n\
</dict>\n\
</plist>\n"
    )
}

pub fn windows_startup_script(exe: &str, with_filesystem: bool) -> String {
    format!(
        "@echo off\r\nstart \"\" \"{exe}\" host{}\r\n",
        remote_args(with_filesystem)
    )
}

/// The `--remote` half of the boot command: present only when the
/// user consented to enabling the filesystem API at every sign-in.
fn remote_args(with_filesystem: bool) -> &'static str {
    if with_filesystem {
        " --remote"
    } else {
        ""
    }
}

/// Interactive consent for putting `--remote` in the boot entry.
///
/// The entry runs unattended forever after install, so consent has to
/// happen here, once, with the consequences spelled out. Fail-closed:
/// EOF or anything that is not an explicit yes installs the relay host
/// without the filesystem API (`--with-filesystem` opts in without a
/// prompt).
pub fn confirm_filesystem_at_boot() -> bool {
    println!(
        "The autostart entry runs `dsterm host` at every sign-in/boot. It can also pass\n\
         --remote, which enables the filesystem API (/fs/*: read, write, delete, list, ...\n\
         over 127.0.0.1 and to approved relay clients) with the boot directory as the\n\
         workspace root, every time it starts."
    );
    print!("Enable the filesystem API in the autostart entry? [y/N] ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut buf = String::new();
    match std::io::stdin().read_line(&mut buf) {
        Ok(_) => consent_allows(&buf),
        Err(_) => false,
    }
}

fn consent_allows(reply: &str) -> bool {
    let t = reply.trim().to_ascii_lowercase();
    t == "y" || t == "yes"
}

pub fn install(with_filesystem: bool) -> anyhow::Result<String> {
    let home = home_dir()?;
    let exe = exe_path();

    if is_termux() {
        let dir = home.join(".termux").join("boot");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("dsterm-host");
        std::fs::write(&path, termux_boot_script(&exe, with_filesystem))?;
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
            std::fs::write(&path, launchd_plist(&exe, with_filesystem))?;
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
            std::fs::write(&path, windows_startup_script(&exe, with_filesystem))?;
            Ok(format!(
                "Installed Startup script at {}. It runs automatically on your next sign-in.",
                path.display()
            ))
        }
        _ => {
            let dir = home.join(".config").join("systemd").join("user");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("dsterm.service");
            std::fs::write(&path, systemd_unit(&exe, with_filesystem))?;
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
        let unit = systemd_unit("/usr/bin/dsterm", true);
        assert!(unit.contains("ExecStart=/usr/bin/dsterm host --remote\n"));
    }

    #[test]
    fn systemd_unit_without_filesystem_runs_plain_host() {
        let unit = systemd_unit("/usr/bin/dsterm", false);
        assert!(unit.contains("ExecStart=/usr/bin/dsterm host\n"));
        assert!(!unit.contains("--remote"));
    }

    #[test]
    fn termux_boot_script_runs_host_remote() {
        let script = termux_boot_script("/usr/bin/dsterm", true);
        assert!(script.contains("/usr/bin/dsterm host --remote &"));
    }

    #[test]
    fn termux_boot_script_without_filesystem_runs_plain_host() {
        let script = termux_boot_script("/usr/bin/dsterm", false);
        assert!(script.contains("/usr/bin/dsterm host &"));
        assert!(!script.contains("--remote"));
    }

    #[test]
    fn launchd_plist_runs_host_remote() {
        let plist = launchd_plist("/opt/dsterm", true);
        assert!(plist.contains("<string>/opt/dsterm</string>"));
        assert!(plist.contains("<string>host</string>"));
        assert!(plist.contains("<string>--remote</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
    }

    #[test]
    fn launchd_plist_without_filesystem_runs_plain_host() {
        let plist = launchd_plist("/opt/dsterm", false);
        assert!(plist.contains("<string>host</string>"));
        assert!(!plist.contains("--remote"));
        assert!(plist.contains("<string>host</string>\n    </array>"));
    }

    #[test]
    fn windows_startup_script_runs_host_remote() {
        let script = windows_startup_script("C:\\dsterm.exe", true);
        assert!(script.contains("start \"\" \"C:\\dsterm.exe\" host --remote"));
        assert!(script.contains("\r\n"));
    }

    #[test]
    fn windows_startup_script_without_filesystem_runs_plain_host() {
        let script = windows_startup_script("C:\\dsterm.exe", false);
        assert!(script.contains("start \"\" \"C:\\dsterm.exe\" host\r\n"));
        assert!(!script.contains("--remote"));
    }

    #[test]
    fn consent_requires_explicit_yes() {
        assert!(consent_allows("y\n"));
        assert!(consent_allows(" Y \n"));
        assert!(consent_allows("yes"));
        assert!(consent_allows("Yes\r\n"));
        assert!(!consent_allows("n\n"));
        assert!(!consent_allows("\n"));
        assert!(!consent_allows(""));
        assert!(!consent_allows("yep"));
    }
}
