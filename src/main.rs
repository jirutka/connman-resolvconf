#![warn(warnings, rust_2018_idioms)]
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::env;
use std::process::exit;
use std::time::Duration;

use anyhow::{anyhow, Context};
use dbus::blocking::Connection;
use log::{error, info, trace, warn, LevelFilter};
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
        let iface = service.interface_or_id();

        info!("Adding DNS information for {} ({})", iface, service.id);
        self.resolvconf.add(iface, &service.resolvconf())?;
        self.services.insert(service.id.clone(), service);

        Ok(())
    }

    fn update(&mut self, id: &str, update: ServiceUpdate) -> anyhow::Result<()> {
        let service = self
            .services
            .get_mut(id)
            .ok_or_else(|| anyhow!("Unknown service: {}", id))?;

        // Update mutates the service.
        if service.update(update) {
            let iface = service.interface_or_id();

            if service.state == "disconnect" {
                info!("Removing DNS information for {} ({})", iface, service.id);
                self.resolvconf.del(iface)?;
                self.services.remove(id);
            } else {
                info!("Updating DNS information for {} ({})", iface, service.id);
                self.resolvconf.add(iface, &service.resolvconf())?;
            }
        }
        Ok(())
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
            "-s" | "--syslog" => {
                args.syslog = true;
            }
            "-V" | "--version" => {
                println!("{} {}", PROG_NAME, PROG_VERSION);
                exit(0)
            }
            "-h" | "--help" => {
                println!(
                    "Usage: {} [--log <level>] [--syslog] [--version] [--help]",
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
        Ok(_) => info!("Terminating"),
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

    let connection = Connection::new_system().context("Failed to connect to the system D-Bus")?;

    let services = Services::new(&connection, Duration::from_millis(5000));
    let mut resolvconf = ResolvconfState::new()?;

    services
        .get_active()?
        .into_iter()
        .try_for_each(|service| resolvconf.insert(service))?;

    services.on_update(move |id, update, services| {
        trace!("Received PropertyChanged: {:?}", update);
        match update {
            ServiceUpdate::State(ref state) if state == "ready" || state == "online" => {
                match services.get(id) {
                    Ok(service) => resolvconf
                        .insert(service)
                        .unwrap_or_else(|e| error!("{:#}", e)),
                    Err(e) => error!("{:#}", e),
                }
            }
            _ => resolvconf
                .update(id, update)
                .unwrap_or_else(|e| error!("{:#}", e))
        };
    })?;

    loop {
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
