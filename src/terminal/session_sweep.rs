//! Best-effort sweep of a terminated PTY's whole session.
//!
//! Closing a terminal kills the shell via the PTY's `ChildKiller`, but
//! an interactive shell runs background jobs (`cargo build &`, dev
//! servers) in their *own* process groups inside the shell's session —
//! neither `kill(shell)` nor `kill(-shell_pgid)` reaches them. The
//! session id survives until the last member exits (the shell calls
//! `setsid`, so sid == shell pid), which makes it the right key to
//! sweep by: walk `/proc`, SIGKILL every live member of that session.
//!
//! The sweep only runs on explicit closes (terminate endpoint, idle
//! eviction); a shell that exits on its own keeps today's semantics.

/// Kills every surviving process in the session `sid` (the PTY shell's
/// pid). No-op off Unix.
pub fn sweep_session(sid: u32) {
    #[cfg(unix)]
    {
        sweep_session_unix(sid);
    }
    #[cfg(not(unix))]
    {
        let _ = sid;
    }
}

#[cfg(unix)]
fn sweep_session_unix(sid: u32) {
    use std::os::unix::ffi::OsStrExt;

    if sid == 0 {
        return;
    }
    let me = std::process::id();
    let entries = match std::fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(sid = %sid, "session sweep: /proc unavailable: {}", err);
            return;
        }
    };
    let mut swept = 0u32;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let bytes = name.as_bytes();
        if bytes.iter().any(|b| !b.is_ascii_digit()) {
            continue;
        }
        let pid = match std::str::from_utf8(bytes)
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
        {
            Some(pid) => pid,
            None => continue,
        };
        if pid == me || pid <= 1 {
            continue;
        }
        let stat = match std::fs::read(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(_) => continue,
        };
        if parse_session_id(&stat) != Some(sid as i32) {
            continue;
        }
        // SIGKILL regardless of liveness: the pid may be a reaped or
        // dying member; ESRCH on those is harmless.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
        swept += 1;
    }
    tracing::info!(sid = %sid, swept = %swept, "session sweep complete");
}

/// Extracts the session id from a `/proc/<pid>/stat` dump.
///
/// Layout after the parenthesised comm (which itself may contain
/// spaces and `)`): `state ppid pgrp session ...` — so parse everything
/// after the **last** `)`. Pure byte parsing, so it stays compiled
/// under `cfg(test)` on every platform: that is how it gets exercised
/// locally on Windows, where the sweep itself cannot run.
#[cfg(any(unix, test))]
fn parse_session_id(stat: &[u8]) -> Option<i32> {
    let close = stat.iter().rposition(|b| *b == b')')?;
    let after = &stat[close + 1..];
    // fields: 0=state 1=ppid 2=pgrp 3=session
    after
        .split(|b| b.is_ascii_whitespace())
        .filter(|f| !f.is_empty())
        .nth(3)
        .and_then(|f| std::str::from_utf8(f).ok())
        .and_then(|s| s.parse::<i32>().ok())
}

#[cfg(test)]
mod tests {
    use super::parse_session_id;

    #[test]
    fn parses_session_id_from_proc_stat() {
        // comm "my) weird (shell" proves the last-paren rule.
        let stat = b"1234 (my) weird (shell) S 1 2222 2222 0 -1 4194304 100 0 0 0 0 0 0 0 20 0 1 0 99 12345 500";
        assert_eq!(parse_session_id(stat), Some(2222));
    }

    #[test]
    fn tolerates_cr_lf_and_missing_fields() {
        let stat = b"9 (x) R 1 55 55\r\n";
        assert_eq!(parse_session_id(stat), Some(55));
        assert_eq!(parse_session_id(b"9 (x) R"), None);
        assert_eq!(parse_session_id(b"no-parens-here"), None);
    }
}
