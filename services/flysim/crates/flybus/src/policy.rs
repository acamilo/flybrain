//! Launcher-provided access policy (bus-v1 section 3): which configured participants may
//! connect, and what each may register, call, publish, subscribe to and manage. Naming a
//! target is not authority to control it.

use std::collections::HashMap;

/// A set of service or topic names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pattern {
    Any,
    Exact(String),
    /// Every name that starts with this string, e.g. `"session.demo."`.
    Prefix(String),
}

impl Pattern {
    pub fn exact(name: &str) -> Pattern {
        Pattern::Exact(name.to_owned())
    }

    pub fn prefix(prefix: &str) -> Pattern {
        Pattern::Prefix(prefix.to_owned())
    }

    pub fn matches(&self, name: &str) -> bool {
        match self {
            Pattern::Any => true,
            Pattern::Exact(n) => n == name,
            Pattern::Prefix(p) => name.starts_with(p.as_str()),
        }
    }
}

/// What one participant may do. Empty lists grant nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grants {
    /// Service names it may register.
    pub register: Vec<Pattern>,
    /// Service names it may call.
    pub call: Vec<Pattern>,
    pub publish: Vec<Pattern>,
    pub subscribe: Vec<Pattern>,
    /// Topic names it may declare, clear and delete.
    pub manage_topics: Vec<Pattern>,
}

impl Grants {
    pub fn all() -> Grants {
        let any = vec![Pattern::Any];
        Grants {
            register: any.clone(),
            call: any.clone(),
            publish: any.clone(),
            subscribe: any.clone(),
            manage_topics: any,
        }
    }

    pub(crate) fn allows(list: &[Pattern], name: &str) -> bool {
        list.iter().any(|p| p.matches(name))
    }
}

/// Who may connect and with what grants.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    clients: HashMap<String, Grants>,
    default: Option<Grants>,
    trusted_unbound: bool,
}

impl Policy {
    /// Admits any client id with every grant. For tests and single-purpose local deployments.
    pub fn open() -> Policy {
        Policy {
            clients: HashMap::new(),
            default: Some(Grants::all()),
            trusted_unbound: true,
        }
    }

    /// Admits only the clients added with [`Policy::client`].
    pub fn closed() -> Policy {
        Policy::default()
    }

    pub fn client(mut self, client_id: &str, grants: Grants) -> Policy {
        self.clients.insert(client_id.to_owned(), grants);
        self
    }

    /// Grants for clients not listed by id; `None` refuses them at hello.
    pub fn with_default(mut self, grants: Option<Grants>) -> Policy {
        self.default = grants;
        self
    }

    pub(crate) fn grants_for(&self, client_id: &str) -> Option<Grants> {
        self.clients
            .get(client_id)
            .cloned()
            .or_else(|| self.default.clone())
    }

    pub(crate) fn permits_unbound_transport(&self) -> bool {
        self.trusted_unbound
    }
}
