#![warn(warnings, rust_2018_idioms)]
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::env;
use std::process::exit;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use dbus::blocking::LocalConnection;
use log::{error, info, trace, warn, LevelFilter};
use signal_hook::consts::SIGTERM;
use syslog::Facility;

use connman::{Service, ServiceUpdate, Services};
use resolvconf::Resolvconf;

mod connman;
mod resolvconf;
mod utils;


const PROG_NAME: &str = env!("CARGO_PKG_NAME");
const PROG_VERSION: &str = env!("CARGO_PKG_VERSION");

struct AppArgs {
    log_filter: String,
    cleanup_on_term: bool,
    syslog: bool,
}

struct ResolvconfState {
    resolvconf: Resolvconf,
    services: HashMap<String, Service>,
}

impl ResolvconfState {
    fn new() -> anyhow::Result<ResolvconfState> {
        Ok(ResolvconfState {
            services: HashMap::new(),
            resolvconf: Resolvconf::new()?,
        })
    }

    fn insert(&mut self, service: Service) -> anyhow::Result<()> {
        // If we already have the given service with the same attributes, do nothing.
        if self.services.get(&service.id).map_or(false, |cur| *cur == service) {
            return Ok(());
        }
        if !service.nameservers.is_empty() {
            let iface = service.interface_or_id();

            info!("Adding DNS information for {} ({})", iface, service.id);
            self.resolvconf.add(iface, &service.resolvconf())?;
        }
        self.services.insert(service.id.clone(), service);

        Ok(())
    }

    fn update(&mut self, id: &str, update: ServiceUpdate) -> anyhow::Result<()> {
        if let Some(service) = self.services.get_mut(id) {
            // Update mutates the service.
            if service.update(&update) {
                let iface = service.interface_or_id();

                match service.state.as_ref() {
                    "ready" | "online" => {
                        info!("Updating DNS information for {} ({})", iface, service.id);
                        self.resolvconf.add(iface, &service.resolvconf())?;
                    }
                    "disconnect" => {
                        info!("Removing DNS information for {} ({})", iface, service.id);
                        self.resolvconf.del(iface)?;
                        self.services.remove(id);
                    }
                    "configuration" => (),  // ignore
                    _ => bail!("Unexpected service update in state {}: {:?}", service.state, update)
                }
            }
        } else {
            trace!("Ignoring update for unknown service: {}", id);
        }
        Ok(())
    }

    fn remove_all(&mut self) {
        for (_, service) in self.services.drain() {
            let iface = service.interface_or_id();

            info!("Removing DNS information for {} ({})", iface, service.id);
            self.resolvconf
                .del(iface)
                .unwrap_or_else(|e| warn!("{:#}", e));
        }
    }
}

impl Service {
    fn resolvconf(&self) -> String {
        let mut buf = String::new();
        buf.push_str(&format!("# Generated for {}\n", self.id));

        if !self.domains.is_empty() {
            buf.push_str(&format!("search {}\n", self.domains.join(" ")));
        }
        for nameserver in self.nameservers.iter() {
            buf.push_str(&format!("nameserver {}\n", nameserver));
        }
        buf
    }
}


fn main() {
    let mut args = AppArgs {
        log_filter: env::var("RUST_LOG").unwrap_or_else(|_| "INFO".into()),
        cleanup_on_term: true,
        syslog: false,
    };

    let mut iter = env::args().skip(1);
    while let Some(opt) = iter.next() {
        match opt.as_str() {
            "-l" | "--log-level" => {
                if let Some(arg) = iter.next() {
                    args.log_filter = arg;
                } else {
                    eprintln!("{}: Option requires an argument: {}", PROG_NAME, opt);
                    exit(100);
                }
            }
            "-C" | "--no-cleanup-on-term" => {
                args.cleanup_on_term = false;
            }
            "-s" | "--syslog" => {
                args.syslog = true;
            }
            "-V" | "--version" => {
                println!("{} {}", PROG_NAME, PROG_VERSION);
                exit(0)
            }
            "-h" | "--help" => {
                println!(
                    "Usage: {} [--log <level>] [--no-cleanup-on-term] [--syslog] [--version] [--help]",
                    PROG_NAME
                );
                exit(0)
            }
            _ => {
                eprintln!("{}: Invalid argument: {}", PROG_NAME, opt);
                exit(100);
            }
        };
    }

    match run(&args) {
        Ok(_) => exit(0),
        Err(e) => {
            error!("{:#}", e);
            if args.syslog {
                eprintln!("{}: {:#}", PROG_NAME, e);
            }
            exit(1)
        }
    };
}

fn run(args: &AppArgs) -> anyhow::Result<()> {
    init_logger(args)?;

    info!("Starting {} {}", PROG_NAME, PROG_VERSION);

    let connection =
        LocalConnection::new_system().context("Failed to connect to the system D-Bus")?;

    let services = Services::new(&connection, Duration::from_millis(5000));
    let mut resolvconf = ResolvconfState::new()?;

    services
        .get_active()?
        .into_iter()
        .try_for_each(|service| resolvconf.insert(service))?;

    let resolvconf = Rc::new(RefCell::new(resolvconf));

    {
        let resolvconf = Rc::clone(&resolvconf);

        services.on_update(move |id, update, services| {
            trace!("Received PropertyChanged: {:?}", update);
            match update {
                ServiceUpdate::State(ref state) if state == "ready" || state == "online" => {
                    match services.get(id) {
                        Ok(service) => resolvconf
                            .borrow_mut()
                            .insert(service)
                            .unwrap_or_else(|e| error!("{:#}", e)),
                        Err(e) => error!("{:#}", e),
                    }
                }
                _ => resolvconf
                    .borrow_mut()
                    .update(id, update)
                    .unwrap_or_else(|e| error!("{:#}", e)),
            };
        })?;
    }

    let sigterm = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGTERM, Arc::clone(&sigterm))
        .context("Failed to register SIGTERM handler")?;

    loop {
        if sigterm.load(Ordering::Relaxed) {
            if args.cleanup_on_term {
                info!("Caught SIGTERM, cleaning up and exiting...");
                resolvconf.borrow_mut().remove_all();
            }
            return Ok(());
        }
        connection.process(Duration::from_millis(1000))?;
    }
}

fn init_logger(args: &AppArgs) -> anyhow::Result<()> {
    if args.syslog {
        let level: LevelFilter = args
            .log_filter
            .parse()
            .map_err(|_| anyhow!("Invalid log level: {}", args.log_filter))?;

        syslog::init_unix(Facility::LOG_DAEMON, level)
            .map_err(|e| anyhow!("Failed to connect to syslog: {}", e))?;
    } else {
        env_logger::builder()
            .parse_filters(&args.log_filter)
            .format_target(false)
            .try_init()?;
    }
    Ok(())
}
