//! Launch at login, without extra dependencies.
//!
//! * macOS: a `LaunchAgent` plist in `~/Library/LaunchAgents`
//! * Windows: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
//! * Linux: an XDG autostart `.desktop` entry
//!
//! Everything is scoped to the current user, and turning it off removes exactly
//! what turning it on created — nothing is left behind in the system.

use std::path::PathBuf;

/// Child processes (`reg`, `netsh`, `powershell`) must never flash a console
/// window: a desktop app that spawns black boxes looks broken.
#[cfg(target_os = "windows")]
trait NoWindow {
    fn quiet(&mut self) -> &mut Self;
}

#[cfg(target_os = "windows")]
impl NoWindow for std::process::Command {
    fn quiet(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        self.creation_flags(CREATE_NO_WINDOW)
    }
}

#[cfg(target_os = "windows")]
fn cmd(program: &str) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command.quiet();
    command
}

#[cfg(target_os = "macos")]
const DOMAIN: &str = "com.velarouter.desktop";
#[cfg(target_os = "windows")]
const WINDOWS_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// The running binary. macOS app bundles must launch through `open -a` so the
/// window server and TCC see a normal app launch.
fn program() -> Option<(PathBuf, bool)> {
    let exe = std::env::current_exe().ok()?;
    #[cfg(target_os = "macos")]
    {
        // .../Velarouter.app/Contents/MacOS/velarouter-desktop
        if let Some(bundle) = exe
            .ancestors()
            .find(|p| p.extension().is_some_and(|e| e == "app"))
        {
            return Some((bundle.to_path_buf(), true));
        }
    }
    Some((exe, false))
}

/// Launching a development build at login is almost never wanted, so the
/// "enabled by default" behaviour only applies to packaged builds.
pub fn is_bundled() -> bool {
    program().is_some_and(|(_, bundled)| bundled)
        || std::env::current_exe()
            .map(|p| !p.to_string_lossy().contains("/target/"))
            .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn plist_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{DOMAIN}.plist")),
    )
}

#[cfg(target_os = "macos")]
pub fn is_enabled() -> bool {
    plist_path().is_some_and(|p| p.is_file())
}

#[cfg(target_os = "macos")]
pub fn set(enabled: bool) -> std::io::Result<()> {
    let Some(path) = plist_path() else {
        return Ok(());
    };
    if !enabled {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    let Some((target, bundled)) = program() else {
        return Ok(());
    };
    let target = xml(&target.to_string_lossy());
    let (program, args) = if bundled {
        (
            "<string>/usr/bin/open</string>".to_string(),
            format!("<string>-a</string><string>{target}</string>"),
        )
    } else {
        (format!("<string>{target}</string>"), String::new())
    };
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{DOMAIN}</string>
  <key>ProgramArguments</key>
  <array>{program}{args}</array>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, plist)
}

#[cfg(target_os = "windows")]
pub fn is_enabled() -> bool {
    cmd("reg")
        .args(["query", WINDOWS_KEY, "/v", "Velarouter"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "windows")]
pub fn set(enabled: bool) -> std::io::Result<()> {
    let status = if enabled {
        let Some((exe, _)) = program() else {
            return Ok(());
        };
        cmd("reg")
            .args([
                "add",
                WINDOWS_KEY,
                "/v",
                "Velarouter",
                "/t",
                "REG_SZ",
                "/d",
                &format!("\"{}\"", exe.display()),
                "/f",
            ])
            .status()?
    } else {
        cmd("reg")
            .args(["delete", WINDOWS_KEY, "/v", "Velarouter", "/f"])
            .status()?
    };
    let _ = status;
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn desktop_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("autostart").join("velarouter.desktop"))
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn is_enabled() -> bool {
    desktop_path().is_some_and(|p| p.is_file())
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn set(enabled: bool) -> std::io::Result<()> {
    let Some(path) = desktop_path() else {
        return Ok(());
    };
    if !enabled {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    let Some((exe, _)) = program() else {
        return Ok(());
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        path,
        format!(
            "[Desktop Entry]\nType=Application\nName=Velarouter\nIcon=Velarouter\nStartupWMClass=Velarouter\nExec=\"{}\"\nX-GNOME-Autostart-enabled=true\n",
            exe.display()
        ),
    )
}

#[cfg(target_os = "macos")]
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Windows Firewall blocks inbound UDP by default for unsigned binaries, which
/// is exactly the traffic hole punching needs. The rule is added for this
/// executable only, and the user sees the UAC prompt — nothing happens silently.
#[cfg(target_os = "windows")]
pub fn allow_firewall() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let script = format!(
        "$exe='{}'; Start-Process -Verb RunAs -FilePath netsh -ArgumentList @(\
         'advfirewall','firewall','add','rule','name=Velarouter',\
         'dir=in','action=allow',\"program=$exe\",'enable=yes')",
        exe.display()
    );
    cmd("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .spawn()?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)] // only the Windows build offers the button
pub fn allow_firewall() -> std::io::Result<()> {
    Ok(())
}

/// True when a firewall rule for this executable already exists.
#[cfg(target_os = "windows")]
pub fn firewall_rule_exists() -> bool {
    cmd("netsh")
        .args(["advfirewall", "firewall", "show", "rule", "name=Velarouter"])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

#[cfg(not(target_os = "windows"))]
pub fn firewall_rule_exists() -> bool {
    true
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn escaping_is_minimal_but_correct() {
        assert_eq!(super::xml("a&b<c>"), "a&amp;b&lt;c&gt;");
    }
}
