//! `sd_notify`, hand-rolled.
//!
//! `infra/units/flysim.service` is `Type=notify` with `NotifyAccess=main` and `WatchdogSec=30`,
//! so the process has to send `READY=1` once it is really serving and `WATCHDOG=1` from inside
//! the simulation loop — a watchdog ping sent from a timer task would keep the unit alive while
//! the loop was wedged, which is the one failure it exists to catch.
//!
//! The protocol is a datagram of `KEY=VALUE` lines to the socket named by `$NOTIFY_SOCKET`,
//! whose path is either a filesystem path or, with a leading `@`, an abstract-namespace name.
//! That is the whole dependency `sd-notify` would have added.

// `from_abstract_name` and `bind_addr`/`connect_addr` are the Linux extension to the portable
// Unix-socket API; systemd only ever names an abstract socket on Linux.
use std::os::linux::net::SocketAddrExt as _;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::time::Duration;

/// Fraction of `WatchdogSec` between pings, as systemd's own documentation recommends.
pub const WATCHDOG_DIVISOR: u32 = 3;
/// Interval used when the unit sets `WatchdogSec` but systemd exported no `WATCHDOG_USEC`.
pub const DEFAULT_WATCHDOG_INTERVAL: Duration = Duration::from_secs(10);

/// A connected notification socket, or nothing when not run under systemd.
#[derive(Debug)]
pub struct Notifier {
    socket: Option<UnixDatagram>,
    watchdog: Option<Duration>,
}

impl Notifier {
    /// Connect to `$NOTIFY_SOCKET` if it is set, and read `$WATCHDOG_USEC`.
    pub fn from_env() -> Self {
        let watchdog = std::env::var("WATCHDOG_USEC")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|micros| *micros > 0)
            .map(|micros| Duration::from_micros(micros / u64::from(WATCHDOG_DIVISOR)));

        let Ok(name) = std::env::var("NOTIFY_SOCKET") else {
            return Self { socket: None, watchdog: None };
        };
        if name.is_empty() {
            return Self { socket: None, watchdog: None };
        }
        match connect(&name) {
            Ok(socket) => {
                tracing::info!(
                    socket = %name,
                    watchdog_interval = ?watchdog,
                    "sd_notify connected"
                );
                Self {
                    socket: Some(socket),
                    watchdog: Some(watchdog.unwrap_or(DEFAULT_WATCHDOG_INTERVAL)),
                }
            }
            Err(error) => {
                tracing::warn!(%error, socket = %name, "could not connect to NOTIFY_SOCKET");
                Self { socket: None, watchdog: None }
            }
        }
    }

    /// A notifier that sends nothing, for tests and for running outside systemd.
    pub fn disabled() -> Self {
        Self { socket: None, watchdog: None }
    }

    /// Whether anything is listening.
    pub fn enabled(&self) -> bool {
        self.socket.is_some()
    }

    /// How often [`Notifier::watchdog`] should be called, or `None` when the unit has no
    /// watchdog.
    pub fn watchdog_interval(&self) -> Option<Duration> {
        self.watchdog
    }

    /// Send one datagram. Failure is logged and ignored: losing a notification must never take
    /// the simulation down.
    pub fn notify(&self, message: &str) {
        let Some(socket) = &self.socket else {
            return;
        };
        if let Err(error) = socket.send(message.as_bytes()) {
            tracing::warn!(%error, "sd_notify send failed");
        }
    }

    pub fn watchdog(&self) {
        self.notify("WATCHDOG=1\n");
    }

    /// `STATUS=` line shown by `systemctl status`.
    pub fn status(&self, status: &str) {
        self.notify(&format!("STATUS={status}\n"));
    }
}

fn connect(name: &str) -> std::io::Result<UnixDatagram> {
    let socket = UnixDatagram::unbound()?;
    if let Some(abstract_name) = name.strip_prefix('@') {
        // systemd spells an abstract-namespace socket `@name`; the kernel wants a leading NUL,
        // which is what `SocketAddr::from_abstract_name` builds.
        let address = SocketAddr::from_abstract_name(abstract_name.as_bytes())?;
        socket.connect_addr(&address)?;
    } else {
        socket.connect(name)?;
    }
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notifier_with_no_socket_is_inert() {
        let notifier = Notifier::disabled();
        assert!(!notifier.enabled());
        assert_eq!(notifier.watchdog_interval(), None);
        // Every call is a no-op rather than an error.
        notifier.notify("READY=1\n");
        notifier.watchdog();
        notifier.status("idle");
    }

    #[test]
    fn a_real_socket_receives_the_protocol_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notify");
        let listener = UnixDatagram::bind(&path).unwrap();

        let socket = connect(path.to_str().unwrap()).unwrap();
        let notifier = Notifier { socket: Some(socket), watchdog: Some(Duration::from_secs(10)) };
        assert!(notifier.enabled());
        notifier.notify("READY=1\nSTATUS=up\n");
        notifier.watchdog();

        let mut buffer = [0u8; 256];
        let read = listener.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..read], b"READY=1\nSTATUS=up\n");
        let read = listener.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..read], b"WATCHDOG=1\n");
    }

    #[test]
    fn an_abstract_socket_name_is_accepted() {
        let name = format!("flysim-test-{}", std::process::id());
        let address = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let listener = UnixDatagram::bind_addr(&address).unwrap();

        let socket = connect(&format!("@{name}")).unwrap();
        Notifier { socket: Some(socket), watchdog: None }.watchdog();

        let mut buffer = [0u8; 64];
        let read = listener.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..read], b"WATCHDOG=1\n");
    }
}
