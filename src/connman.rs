use std::time::Duration;

use anyhow::anyhow;
use dbus::arg;
use dbus::blocking::Connection;
use dbus::message::MatchRule;
use log::{error, warn};


const BUS_NAME: &str = "net.connman";

mod net_connman {
    use std::ops::Deref;

    use dbus::arg::PropMap;
    use dbus::blocking::{BlockingSender, Proxy};

    pub trait Manager {
        fn get_services(&self) -> Result<Vec<(dbus::Path<'static>, PropMap)>, dbus::Error>;
    }

    impl<'a, T: BlockingSender, C: Deref<Target = T>> Manager for Proxy<'a, C> {
        fn get_services(&self) -> Result<Vec<(dbus::Path<'static>, PropMap)>, dbus::Error> {
            self.method_call("net.connman.Manager", "GetServices", ())
                .map(|rec: (Vec<(dbus::Path<'static>, PropMap)>,)| rec.0)
        }
    }
}


#[derive(Clone, Debug)]
pub struct Service {
    /// Service ID (the last part of the service D-Bus path)
    pub id: String,
    /// Connection state
    pub state: String,
    /// Network interface
    pub interface: Option<String>,
    /// List of currently-active nameservers
    pub nameservers: Vec<String>,
    /// List of currently-used search domains
    pub domains: Vec<String>,
}

impl Service {
    pub fn interface_or_id(&self) -> &str {
        self.interface.as_ref().unwrap_or(&self.id)
    }

    pub fn update(&mut self, change: ServiceUpdate) -> bool {
        use ServiceUpdate::*;
        match change {
            State(value) if self.state != value => {
                self.state = value
            }
            Domains(value) if self.domains != value => {
                self.domains = value
            }
            Nameservers(value) if self.nameservers != value => {
                self.nameservers = value
            }
            _ => return false
        };
        true
    }
}

impl TryFrom<(dbus::Path<'_>, arg::PropMap)> for Service {
    type Error = anyhow::Error;

    fn try_from(value: (dbus::Path<'_>, arg::PropMap)) -> Result<Service, Self::Error> {
        let (path, ref props) = value;

        let prefix = Services::SERVICE_PATH_PREFIX;
        let id = path
            .strip_prefix(prefix)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("Expected path with prefix {}, but got {}", prefix, path))?;

        let state = arg::prop_cast::<String>(props, "State")
            .map(Clone::clone)
            .ok_or_else(|| anyhow!("{} is missing property 'State'", path))?;

        let interface = arg::prop_cast::<arg::PropMap>(props, "Ethernet")
            .and_then(|p| arg::prop_cast::<String>(p, "Interface"))
            .map(Clone::clone);

        let nameservers = arg::prop_cast::<Vec<String>>(props, "Nameservers")
            .map_or(vec![], Clone::clone);

        let domains = arg::prop_cast::<Vec<String>>(props, "Domains")
            .map_or(vec![], Clone::clone);

        Ok(Service {
            id,
            state,
            interface,
            nameservers,
            domains,
        })
    }
}


#[derive(Debug)]
pub enum ServiceUpdate {
    /// List of currently-used search domains
    Domains(Vec<String>),
    /// List of currently-active name servers
    Nameservers(Vec<String>),
    /// Connection state
    State(String),
    /// Any other property we currently don't use here
    Other,
}

impl arg::ReadAll for ServiceUpdate {
    fn read(iter: &mut arg::Iter<'_>) -> Result<Self, arg::TypeMismatchError> {
        match iter.read()? {
            "Domains" => iter
                .read::<arg::Variant<Vec<String>>>()
                .map(|var| ServiceUpdate::Domains(var.0)),
            "Nameservers" => iter
                .read::<arg::Variant<Vec<String>>>()
                .map(|var| ServiceUpdate::Nameservers(var.0)),
            "State" => iter
                .read::<arg::Variant<String>>()
                .map(|var| ServiceUpdate::State(var.0)),
            _ => Ok(ServiceUpdate::Other),
        }
    }
}


pub struct Services<'a> {
    proxy: dbus::blocking::Proxy<'a, &'a Connection>,
}

impl<'a> Services<'a> {
    const SERVICE_PATH_PREFIX: &'static str = "/net/connman/service/";

    pub fn new(connection: &'a Connection, timeout: Duration) -> Services<'a> {
        Services {
            proxy: connection.with_proxy(BUS_NAME, "/", timeout),
        }
    }

    pub fn get_active(&self) -> anyhow::Result<Vec<Service>> {
        use net_connman::Manager;
        let services = self
            .proxy
            .get_services()?
            .into_iter()
            .filter_map(|rec| match Service::try_from(rec) {
                Ok(o) => Some(o),
                Err(e) => {
                    warn!("{:#}", e);
                    None
                }
            })
            .filter(|s| s.state == "online" || s.state == "ready")
            .collect();
        Ok(services)
    }

    pub fn get(&self, id: &str) -> anyhow::Result<Service> {
        let path: dbus::Path<'_> = format!("{}{}", Self::SERVICE_PATH_PREFIX, id).into();

        use net_connman::Manager;
        self.proxy
            .get_services()?
            .into_iter()
            .find(|t| t.0 == path)
            .ok_or_else(|| anyhow!("No such service found: {}", id))
            .and_then(Service::try_from)
    }

    pub fn on_update<F>(&self, mut callback: F) -> Result<dbus::channel::Token, dbus::Error>
    where
        F: FnMut(&str, ServiceUpdate, Services<'_>) + Send + 'static,
    {
        let rule =
            MatchRule::new_signal("net.connman.Service", "PropertyChanged").with_sender(BUS_NAME);
        let timeout = self.proxy.timeout;

        self.proxy
            .connection
            .add_match(rule, move |value: ServiceUpdate, conn, msg| {
                if let ServiceUpdate::Other = value {
                    return true;
                }
                let path = msg.path().unwrap_or_else(|| {
                    error!("Got DBus Message without a path");
                    panic!("Got DBus Message without a path");
                });

                match path.strip_prefix(Self::SERVICE_PATH_PREFIX) {
                    Some(id) => callback(id, value, Services::new(conn, timeout)),
                    None => warn!("Received DBus Message with unexpected path: {}", path),
                };
                true
            })
    }
}
