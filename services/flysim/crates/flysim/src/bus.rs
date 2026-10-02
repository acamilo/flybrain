//! The embedded flybus router: one router under `feed.bus_dir` for whatever rides the bus
//! (`FLY_FEED_VIA=bus`, `FLY_CONTROL_VIA=bus`, or both).
//!
//! The feed's half is `crate::feedbus` (its topic, its policy, its limits and `edge.sock`), the
//! control's half is `crate::controlbus` (its services, its grant table and its sockets); this
//! module only composes them on one router, so a box with both on the bus runs one router, one
//! store and one closed policy.

use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;

use anyhow::{Context as _, Result};
use flybus::{Limits, Policy, Router, RouterConfig, UnixListenerHandle};

use crate::controlbus::{self, Role, Scope};
use crate::feedbus;

/// What rides the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Uses {
    pub feed: bool,
    pub control: bool,
}

/// A running router and its sockets. Dropping it stops listening; the router stops with the
/// runtime it was started on.
pub struct EmbeddedBus {
    pub router: Router,
    _listeners: Vec<UnixListenerHandle>,
}

/// The router limits: the feed's (`docs/design/flybus.md`, amendment "Feed sizing"), with room
/// for the control participants when control rides the bus too: the in-process host, the edge
/// and one connection per native role, pending handshakes included, and the control services.
pub fn limits(uses: Uses) -> Limits {
    let feed = feedbus::limits();
    if !uses.control {
        return feed;
    }
    Limits {
        max_clients: feed.max_clients + 8,
        max_services: feed.max_services + 8,
        ..feed
    }
}

/// The closed policy for `uses`.
pub fn policy(uses: Uses, scope: &Scope) -> Policy {
    let base = if uses.feed {
        feedbus::policy()
    } else {
        Policy::closed()
    };
    if uses.control {
        controlbus::policy(base, scope)
    } else {
        base
    }
}

fn private_dir(path: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("creating {}", path.display()))
}

fn remove_stale(socket: &Path) -> Result<()> {
    match std::fs::remove_file(socket) {
        Ok(()) => {
            tracing::info!(socket = %socket.display(), "removed a stale bus socket");
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", socket.display())),
    }
}

/// Start the router under `bus_dir` and listen on every socket `uses` needs: `edge.sock` for the
/// feed edge, `control-edge.sock` for the control edge and `control/<role>.sock` for each native
/// control role, each launcher-bound to its participant.
///
/// Must run inside a Tokio runtime. `Router::new` removes store directories a previous flysim
/// left behind; stale socket files are removed here, because a socket outlives its listener.
pub async fn start(bus_dir: &Path, uses: Uses, scope: &Scope) -> Result<EmbeddedBus> {
    private_dir(bus_dir)?;
    let root = feedbus::store_root(bus_dir);
    private_dir(&root)?;
    let mut config = RouterConfig::new(root);
    config.limits = limits(uses);
    config.policy = policy(uses, scope);
    let router = Router::new(config).context("starting the flybus router")?;

    let mut sockets = Vec::new();
    if uses.feed {
        sockets.push((feedbus::socket_path(bus_dir), feedbus::EDGE));
    }
    if uses.control {
        private_dir(&bus_dir.join(controlbus::ROLE_SOCKET_DIR))?;
        for role in Role::ALL {
            sockets.push((controlbus::socket_path(bus_dir, role), role.participant()));
        }
    }
    let mut listeners = Vec::new();
    for (socket, participant) in sockets {
        remove_stale(&socket)?;
        listeners.push(
            router
                .listen_unix_as(&socket, participant)
                .await
                .with_context(|| format!("listening on {}", socket.display()))?,
        );
    }
    tracing::info!(
        bus = %bus_dir.display(),
        feed = uses.feed,
        control = uses.control,
        store = %router.store_dir().display(),
        router = router.router_id(),
        "bus listening"
    );
    Ok(EmbeddedBus {
        router,
        _listeners: listeners,
    })
}
