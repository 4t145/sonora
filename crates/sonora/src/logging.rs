use std::fs::{self, File, OpenOptions};

use env_logger::{Env, Logger, Target};
use log::{Log, Metadata, Record};

const CONSOLE: &str = "warn,symphonia=error,lofty=error";
const DISK: &str =
    "warn,symphonia=error,lofty=error,sonora=debug,ui=debug,music=debug,ytmusic=debug";
const FILTER: &str = "SONORA_LOG";
const PREVIOUS: &str = "sonora.log.1";
const LIMIT: u64 = 8 * 1024 * 1024;

pub fn init() {
    let console = env_logger::Builder::from_env(Env::default().default_filter_or(CONSOLE))
        .format_timestamp(None)
        .format_module_path(false)
        .build();

    let disk = open().map(|file| {
        env_logger::Builder::from_env(Env::new().filter_or(FILTER, DISK))
            .target(Target::Pipe(Box::new(file)))
            .build()
    });

    let (level, logger): (log::LevelFilter, Box<dyn Log>) = match disk {
        None => (console.filter(), Box::new(console)),
        Some(disk) => (
            console.filter().max(disk.filter()),
            Box::new(Fan { console, disk }),
        ),
    };

    if log::set_boxed_logger(logger).is_ok() {
        log::set_max_level(level);
    }

    log::debug!("logging: sonora {} started", env!("CARGO_PKG_VERSION"));
}

fn open() -> Option<File> {
    let path = state::log_file()?;
    let folder = path.parent()?;
    fs::create_dir_all(folder).ok()?;

    let outgrown = fs::metadata(&path).is_ok_and(|file| file.len() > LIMIT);
    if outgrown {
        let _ = fs::rename(&path, folder.join(PREVIOUS));
    }

    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()
}

struct Fan {
    console: Logger,
    disk: Logger,
}

impl Log for Fan {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.console.enabled(metadata) || self.disk.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        self.console.log(record);
        self.disk.log(record);
    }

    fn flush(&self) {
        self.console.flush();
        self.disk.flush();
    }
}
