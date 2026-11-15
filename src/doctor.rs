//! What the engraver needs from the system, and how to provide what is missing.
//!
//! The program itself has no extra dependency: the fonts are built in and the Linux driver
//! of the USB-serial converter (`ch341`) is part of the kernel. Two things can still be
//! missing, and both belong to the system:
//!
//! - the **driver** is not bound to the converter: the USB device is there but no serial
//!   port appears (module not loaded, or `brltty` took the converter for a braille display);
//! - the **permission** to open the port: it belongs to root and a group (`uucp` on Arch,
//!   `dialout` on Debian and Ubuntu). The udev rule shipped with this program gives access
//!   to the user logged in at the computer.
//!
//! Detection only reads `/sys` and asks the kernel about permissions: it never opens the port.

use std::path::{Path, PathBuf};

use crate::device::CH340_IDS;

/// The udev rule of this project, installed by the packages and by [`Problem::fix`].
pub const UDEV_RULE: &str = include_str!("../packaging/linux/70-neje.rules");
pub const UDEV_RULE_PATH: &str = "/etc/udev/rules.d/70-neje.rules";

/// Something the system lacks for the engraver to work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// The converter is plugged in but has no serial port.
    DriverNotBound {
        /// True when `brltty` is installed: it claims CH340 converters as braille displays.
        brltty: bool,
    },
    /// The serial port exists but this user may not open it.
    NoAccess { port: String },
}

/// A command that solves a [`Problem`]; it must run as root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    /// Shell script for `sh -c`.
    pub script: String,
    /// A file the script needs: path and content, written before running it.
    pub file: Option<(PathBuf, &'static str)>,
}

impl Problem {
    /// The command that solves the problem, when there is one the program can run.
    pub fn fix(&self) -> Option<Fix> {
        match self {
            Problem::DriverNotBound { brltty: true } => None, // removing a package is the user's call
            Problem::DriverNotBound { brltty: false } => Some(Fix { script: "modprobe ch341".to_owned(), file: None }),
            Problem::NoAccess { .. } => {
                let rule = std::env::temp_dir().join("70-neje.rules");
                let script = format!(
                    "install -Dm644 '{}' {UDEV_RULE_PATH} && udevadm control --reload && udevadm trigger --subsystem-match=tty",
                    rule.display()
                );
                Some(Fix { script, file: Some((rule, UDEV_RULE)) })
            }
        }
    }
}

impl Fix {
    /// Runs the fix as root through `elevator` (`pkexec` asks for the password in a window,
    /// `sudo` in the terminal).
    pub fn run(&self, elevator: &str) -> Result<(), String> {
        if let Some((path, content)) = &self.file {
            std::fs::write(path, content).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        let status = std::process::Command::new(elevator)
            .args(["sh", "-c", &self.script])
            .status()
            .map_err(|e| format!("{elevator}: {e}"))?;
        if status.success() { Ok(()) } else { Err(format!("{elevator}: {status}")) }
    }
}

/// Looks for what is missing. An unplugged engraver is not a problem: the list is empty.
pub fn check() -> Vec<Problem> {
    check_in(Path::new("/sys/bus/usb/devices"), Path::new("/dev"))
}

/// [`check`] on the given `/sys/bus/usb/devices` and `/dev` folders.
fn check_in(usb_devices: &Path, dev: &Path) -> Vec<Problem> {
    let mut problems = Vec::new();
    for device in read_dir(usb_devices) {
        let id = |name: &str| {
            std::fs::read_to_string(device.join(name)).ok().and_then(|s| u16::from_str_radix(s.trim(), 16).ok())
        };
        let (Some(vendor), Some(product)) = (id("idVendor"), id("idProduct")) else { continue };
        if !CH340_IDS.contains(&(vendor, product)) {
            continue;
        }
        // The serial port is a `ttyUSBn` folder inside one of the interfaces of the device.
        let ports: Vec<String> = read_dir(&device)
            .iter()
            .flat_map(|interface| read_dir(interface))
            .filter_map(|entry| entry.file_name().map(|name| name.to_string_lossy().into_owned()))
            .filter(|name| name.starts_with("ttyUSB") || name.starts_with("ttyACM"))
            .collect();
        if ports.is_empty() {
            problems.push(Problem::DriverNotBound { brltty: brltty_installed() });
        }
        for port in ports {
            let path = dev.join(&port);
            if path.exists() && !can_open(&path) {
                problems.push(Problem::NoAccess { port: path.display().to_string() });
            }
        }
    }
    problems
}

fn read_dir(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(|entries| entries.flatten().map(|e| e.path()).collect()).unwrap_or_default()
}

fn brltty_installed() -> bool {
    ["/usr/bin/brltty", "/sbin/brltty", "/usr/sbin/brltty"].iter().any(|p| Path::new(p).exists())
}

/// True when this user may read and write `path` (groups and ACLs included), without
/// opening it.
#[cfg(unix)]
fn can_open(path: &Path) -> bool {
    use nix::unistd::{AccessFlags, access};
    access(path, AccessFlags::R_OK | AccessFlags::W_OK).is_ok()
}

#[cfg(not(unix))]
fn can_open(_path: &Path) -> bool {
    true
}

/// The first of `pkexec` and `sudo` found in `PATH`; `graphical` prefers the one that asks
/// for the password in a window.
pub fn elevator(graphical: bool) -> Option<&'static str> {
    let order = if graphical { ["pkexec", "sudo"] } else { ["sudo", "pkexec"] };
    let path = std::env::var_os("PATH")?;
    order.into_iter().find(|program| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;

    /// A fake `/sys/bus/usb/devices` with one device, optionally with a serial port.
    fn fake_sys(name: &str, vendor: &str, product: &str, port: Option<&str>) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("neje-doctor-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (sys, dev) = (root.join("sys"), root.join("dev"));
        let interface = sys.join("1-3").join("1-3:1.0");
        fs::create_dir_all(&interface).unwrap();
        fs::create_dir_all(&dev).unwrap();
        fs::write(sys.join("1-3").join("idVendor"), format!("{vendor}\n")).unwrap();
        fs::write(sys.join("1-3").join("idProduct"), format!("{product}\n")).unwrap();
        if let Some(port) = port {
            fs::create_dir_all(interface.join(port)).unwrap();
        }
        (sys, dev)
    }

    #[test]
    fn converter_without_a_port_needs_the_driver() {
        let (sys, dev) = fake_sys("driver", "1a86", "7523", None);
        assert!(matches!(check_in(&sys, &dev)[..], [Problem::DriverNotBound { .. }]));
    }

    #[test]
    fn unreadable_port_needs_the_rule() {
        use std::os::unix::fs::PermissionsExt;
        let (sys, dev) = fake_sys("access", "1a86", "7523", Some("ttyUSB7"));
        let port = dev.join("ttyUSB7");
        fs::write(&port, "").unwrap();
        assert_eq!(check_in(&sys, &dev), [], "a port this user can open is fine");
        fs::set_permissions(&port, fs::Permissions::from_mode(0o000)).unwrap();
        // Root can open anything: the check is meaningless there.
        if !can_open(&port) {
            assert_eq!(check_in(&sys, &dev), [Problem::NoAccess { port: port.display().to_string() }]);
        }
    }

    #[test]
    fn other_devices_and_missing_engraver_are_not_problems() {
        let (sys, dev) = fake_sys("other", "046d", "c52b", None);
        assert_eq!(check_in(&sys, &dev), []);
        assert_eq!(check_in(Path::new("/nonexistent"), &dev), []);
    }

    #[test]
    fn the_fix_installs_the_rule_through_the_elevator() {
        let dir = std::env::temp_dir().join(format!("neje-doctor-fix-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // A stand-in for pkexec that records what it is asked to run.
        let fake = dir.join("fake-pkexec");
        let log = dir.join("log");
        fs::write(&fake, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", log.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();

        let fix = Problem::NoAccess { port: "/dev/ttyUSB0".into() }.fix().unwrap();
        fix.run(fake.to_str().unwrap()).unwrap();
        let asked = fs::read_to_string(&log).unwrap();
        assert!(asked.starts_with("sh\n-c\n"), "{asked}");
        assert!(asked.contains("/etc/udev/rules.d/70-neje.rules") && asked.contains("udevadm trigger"));
        let (rule, _) = fix.file.unwrap();
        assert!(fs::read_to_string(rule).unwrap().contains("TAG+=\"uaccess\""));
        assert!(!UDEV_RULE.contains("0666"));
    }

    #[test]
    fn brltty_has_no_automatic_fix() {
        assert_eq!(Problem::DriverNotBound { brltty: true }.fix(), None);
        assert_eq!(Problem::DriverNotBound { brltty: false }.fix().unwrap().script, "modprobe ch341");
    }
}
