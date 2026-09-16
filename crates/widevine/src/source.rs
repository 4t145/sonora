//! Where the CDM comes from.
//!
//! In the order Kodi's InputStream Helper uses: a copy the user pointed at, a copy a browser on
//! the machine already has, then the one Sonora fetched from Google into its own store. The
//! browser search reads fixed paths, one per vendor, and only ever lists a folder the browser
//! itself owns: its `WidevineCdm` component folder for the version, its profile folder for a
//! Firefox-family profile, its app bundle on macOS for the framework version. It never lists
//! `~/.config`, `/Applications` or anything else shared, since a scan over folders that are not
//! ours is what an antivirus flags.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context as _, Result};

use crate::{CDM_PATH, SKIP_BROWSERS};

/// The file name the CDM has on this platform.
#[cfg(target_os = "windows")]
pub const LIBRARY: &str = "widevinecdm.dll";
#[cfg(target_os = "macos")]
pub const LIBRARY: &str = "libwidevinecdm.dylib";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub const LIBRARY: &str = "libwidevinecdm.so";

/// The operating system as the update service and the `_platform_specific` folders name it.
#[cfg(target_os = "windows")]
pub(crate) const OS: &str = "win";
#[cfg(target_os = "macos")]
pub(crate) const OS: &str = "mac";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub(crate) const OS: &str = "linux";

/// The processor as they name it, empty on a machine Google builds no CDM for.
#[cfg(target_arch = "x86_64")]
pub(crate) const ARCH: &str = "x64";
#[cfg(target_arch = "aarch64")]
pub(crate) const ARCH: &str = "arm64";
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub(crate) const ARCH: &str = "";

/// How a CDM was come by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Named by `SONORA_WIDEVINE_CDM`.
    Configured,
    /// A copy a browser on this machine already had.
    Installed,
    /// Fetched from Google's update service into Sonora's own store.
    Fetched,
}

/// A CDM on disk and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub path: PathBuf,
    pub origin: Origin,
}

/// A place to look: a folder that exists on this platform and the names below it, where a `*`
/// lists that one level. Every `*` sits under a folder the browser owns.
struct Place {
    base: PathBuf,
    under: Vec<String>,
}

/// The CDM this process settled on. Only a search that found something is remembered: with
/// nothing found the next call looks again, so a module fetched part way through a run is
/// picked up without a restart. [`uninstall`] clears it.
static FOUND: Mutex<Option<Found>> = Mutex::new(None);

fn settled() -> MutexGuard<'static, Option<Found>> {
    FOUND
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The CDM this process would use, or nothing when the machine has none. Cheap after the first
/// call, which matters because every track asks.
pub fn find() -> Option<Found> {
    if let Some(found) = settled().as_ref() {
        return Some(found.clone());
    }
    let found = search()?;
    Some(settled().get_or_insert(found).clone())
}

/// Remembers a module the store just gained, unless the process has settled on one already.
pub(crate) fn remember(found: Found) -> Found {
    settled().get_or_insert(found).clone()
}

/// Removes every module Sonora fetched from Google, store folder and all, and forgets the one
/// the process settled on so the next search starts over. A CDM already open stays open until
/// the process ends: the file is unlinked, not unloaded.
pub fn uninstall() -> Result<()> {
    let store = store();
    match std::fs::remove_dir_all(&store) {
        Ok(()) => log::info!("widevine: removed the store at {}", store.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("cannot remove {}", store.display()));
        }
    }
    *settled() = None;
    Ok(())
}

/// The environment, then a browser's copy, then the store.
fn search() -> Option<Found> {
    if let Some(path) = configured() {
        return Some(Found {
            path,
            origin: Origin::Configured,
        });
    }
    if let Some(path) = installed() {
        return Some(Found {
            path,
            origin: Origin::Installed,
        });
    }
    stored().map(|path| Found {
        path,
        origin: Origin::Fetched,
    })
}

/// The path `SONORA_WIDEVINE_CDM` names, if it names a file that is there.
pub fn configured() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os(CDM_PATH)?);
    path.is_file().then_some(path)
}

/// The newest CDM a browser on this machine has, searched in the order of [`places`]: the
/// first place that has one answers. Nothing when `SONORA_WIDEVINE_SKIP_BROWSERS` is set.
pub fn installed() -> Option<PathBuf> {
    if std::env::var_os(SKIP_BROWSERS).is_some_and(|value| !value.is_empty()) {
        log::debug!("widevine: skipping the browser search as asked");
        return None;
    }
    places()
        .into_iter()
        .find_map(|place| newest(hunt(place.base, &place.under)))
}

/// Sonora's own folder for the module, `$XDG_CACHE_HOME/sonora/widevine`, holding one
/// subfolder per version fetched.
pub fn store() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("sonora")
        .join("widevine")
}

/// The module of the newest version in the store.
pub fn stored() -> Option<PathBuf> {
    newest(hunt(store(), &steps(&format!("*/{LIBRARY}"))))
}

/// The component layout below a version folder.
fn component() -> String {
    format!("_platform_specific/{OS}_{ARCH}/{LIBRARY}")
}

/// Where a browser's CDM may be. A Chromium-family browser either bundles the component beside
/// the browser or lets its component updater put it under its own config folder; a Firefox
/// keeps its copy in the profile that fetched it.
#[cfg(target_os = "linux")]
fn places() -> Vec<Place> {
    let component = component();
    let mut places = Vec::new();
    for vendor in [
        "google/chrome",
        "google/chrome-beta",
        "google/chrome-unstable",
        "microsoft/msedge",
        "microsoft/msedge-beta",
        "brave.com/brave",
        "vivaldi",
    ] {
        places.push(place(
            "/opt",
            &format!("{vendor}/WidevineCdm/*/{component}"),
        ));
    }
    for lib in ["/usr/lib", "/usr/lib64"] {
        for vendor in ["chromium", "chromium-browser", "opera", "vivaldi"] {
            places.push(place(lib, &format!("{vendor}/WidevineCdm/*/{component}")));
        }
    }
    let Some(home) = dirs::home_dir() else {
        return places;
    };
    let config = home.join(".config");
    for vendor in [
        "google-chrome",
        "google-chrome-beta",
        "google-chrome-unstable",
        "chromium",
        "microsoft-edge",
        "microsoft-edge-beta",
        "BraveSoftware/Brave-Browser",
        "vivaldi",
        "opera",
    ] {
        places.push(place(
            &config,
            &format!("{vendor}/WidevineCdm/*/{component}"),
        ));
    }
    let flatpak = home.join(".var/app");
    for vendor in [
        "com.google.Chrome/config/google-chrome",
        "org.chromium.Chromium/config/chromium",
        "com.microsoft.Edge/config/microsoft-edge",
        "com.brave.Browser/config/BraveSoftware/Brave-Browser",
        "com.vivaldi.Vivaldi/config/vivaldi",
    ] {
        places.push(place(
            &flatpak,
            &format!("{vendor}/WidevineCdm/*/{component}"),
        ));
    }
    for profiles in [
        home.join(".mozilla/firefox"),
        home.join(".librewolf"),
        home.join(".zen"),
        home.join(".waterfox"),
        flatpak.join("org.mozilla.firefox/.mozilla/firefox"),
        flatpak.join("io.gitlab.librewolf-community/.librewolf"),
        home.join("snap/firefox/common/.mozilla/firefox"),
    ] {
        places.push(place(profiles, &format!("*/gmp-widevinecdm/*/{LIBRARY}")));
    }
    places
}

/// The macOS places. A Chromium-family browser keeps the component inside the versioned
/// framework of its own bundle; its component updater keeps a newer one under Application
/// Support.
#[cfg(target_os = "macos")]
fn places() -> Vec<Place> {
    let component = component();
    let mut places = Vec::new();
    let bundles = [
        "Google Chrome",
        "Google Chrome Beta",
        "Google Chrome Canary",
        "Chromium",
        "Brave Browser",
        "Microsoft Edge",
        "Vivaldi",
        "Opera",
        "Arc",
    ];
    let mut roots = vec![PathBuf::from("/Applications")];
    let home = dirs::home_dir();
    if let Some(home) = &home {
        roots.push(home.join("Applications"));
    }
    for root in &roots {
        for bundle in bundles {
            places.push(place(
                root,
                &format!(
                    "{bundle}.app/Contents/Frameworks/*/Versions/*/Libraries/WidevineCdm/{component}"
                ),
            ));
        }
    }
    let Some(home) = home else {
        return places;
    };
    let support = home.join("Library/Application Support");
    for vendor in [
        "Google/Chrome",
        "Google/Chrome Beta",
        "Google/Chrome Canary",
        "Chromium",
        "BraveSoftware/Brave-Browser",
        "Microsoft Edge",
        "Vivaldi",
        "com.operasoftware.Opera",
        "Arc/User Data",
    ] {
        places.push(place(
            &support,
            &format!("{vendor}/WidevineCdm/*/{component}"),
        ));
    }
    for vendor in ["Firefox", "zen", "LibreWolf", "Waterfox"] {
        places.push(place(
            &support,
            &format!("{vendor}/Profiles/*/gmp-widevinecdm/*/{LIBRARY}"),
        ));
    }
    places
}

/// The Windows places. Edge is part of the system, so its module is on every machine already;
/// the others keep theirs the same way, beside the versioned application and under the
/// browser's user data.
#[cfg(target_os = "windows")]
fn places() -> Vec<Place> {
    let component = component();
    let mut places = Vec::new();
    for root in ["ProgramFiles", "ProgramFiles(x86)"] {
        let Some(base) = std::env::var_os(root).map(PathBuf::from) else {
            continue;
        };
        for vendor in [
            "Microsoft/Edge",
            "Microsoft/Edge Beta",
            "Google/Chrome",
            "Google/Chrome Beta",
            "BraveSoftware/Brave-Browser",
            "Vivaldi",
            "Chromium",
        ] {
            places.push(place(
                &base,
                &format!("{vendor}/Application/*/WidevineCdm/{component}"),
            ));
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        for vendor in [
            "Microsoft/Edge",
            "Google/Chrome",
            "Google/Chrome Beta",
            "BraveSoftware/Brave-Browser",
            "Vivaldi",
            "Chromium",
        ] {
            places.push(place(
                &local,
                &format!("{vendor}/User Data/WidevineCdm/*/{component}"),
            ));
        }
    }
    if let Some(roaming) = std::env::var_os("APPDATA").map(PathBuf::from) {
        for vendor in ["Mozilla/Firefox", "zen", "librewolf", "Waterfox"] {
            places.push(place(
                &roaming,
                &format!("{vendor}/Profiles/*/gmp-widevinecdm/*/{LIBRARY}"),
            ));
        }
    }
    places
}

/// Every other platform has nowhere to look, which is the same answer as having no host.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn places() -> Vec<Place> {
    Vec::new()
}

fn place(base: impl Into<PathBuf>, under: &str) -> Place {
    Place {
        base: base.into(),
        under: steps(under),
    }
}

/// Splits a slash-separated tail into the names [`hunt`] walks.
fn steps(under: &str) -> Vec<String> {
    under.split('/').map(str::to_string).collect()
}

/// Every existing path under `base` that matches `under`, where a `*` is one directory name.
/// A fixed name is joined, never listed, so the only folders read are the ones a `*` names.
fn hunt(base: PathBuf, under: &[String]) -> Vec<PathBuf> {
    let Some((head, tail)) = under.split_first() else {
        return match base.is_file() {
            true => vec![base],
            false => Vec::new(),
        };
    };
    if head != "*" {
        return hunt(base.join(head), tail);
    }
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    entries
        .flatten()
        .flat_map(|entry| hunt(entry.path(), tail))
        .collect()
}

/// The path holding the highest version number, so several copies of different ages pick the
/// newest rather than whichever the filesystem listed first.
fn newest(paths: Vec<PathBuf>) -> Option<PathBuf> {
    paths.into_iter().max_by_key(|path| version_of(path))
}

/// The version a path carries, read from the deepest folder that is nothing but numbers and
/// dots. A path with no such folder sorts below every path that has one.
fn version_of(path: &Path) -> Vec<u64> {
    path.components()
        .rev()
        .filter_map(|part| part.as_os_str().to_str())
        .find_map(version)
        .unwrap_or_default()
}

/// A folder name that is nothing but numbers and dots, as a version that sorts.
pub(crate) fn version(name: &str) -> Option<Vec<u64>> {
    let parts: Option<Vec<u64>> = name.split('.').map(|part| part.parse().ok()).collect();
    parts.filter(|parts| parts.len() > 1)
}
