//! Whether this machine can decrypt protected tracks.
//!
//! Sonora ships no Widevine module: Google publishes nothing anyone may redistribute. It does
//! what Kodi does instead. A copy a browser on the machine already has is used as it lies.
//! With none, and only once an account is here for a provider whose tracks need the module,
//! the user is asked whether to download it from Google, shown Google's terms out of the
//! downloaded archive, and asked again before it is installed into Sonora's own store. A
//! module accepted that way is kept current without asking again. With nothing accepted,
//! protected providers keep their metadata and refuse to play.

use gpui::{Context, Entity, Task};
use music::drm::{self, Offer, Origin};

use crate::{Io, Session, SessionEvent};

/// Google's terms for a downloaded module, as the prompt shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terms {
    pub version: String,
    pub license: String,
}

/// Where the module stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CdmState {
    /// Reading the environment, the browsers and the store, which is only the first moment of
    /// a run.
    Looking,
    /// One is here, and where it came from.
    Ready(Origin),
    /// Nothing is here and a protected provider has an account: the user is being asked
    /// whether to download.
    Wanted,
    /// Downloading from Google, to show the terms.
    Offering,
    /// Downloaded; the user is being asked to accept the terms.
    Offered(Terms),
    /// Accepted and going into the store.
    Installing,
    /// The user said no this run. Settings still offers the download.
    Declined,
    /// Nothing is here and nothing is being asked for.
    Missing,
}

/// The Widevine module as app state.
pub struct Drm {
    state: CdmState,
    offer: Option<Offer>,
    session: Entity<Session>,
    io: Io,
    task: Option<Task<()>>,
}

impl Drm {
    pub fn new(session: Entity<Session>, io: Io, cx: &mut Context<Self>) -> Self {
        cx.subscribe(&session, |this, session, event, cx| {
            if matches!(event, SessionEvent::SignedIn)
                && drm::supported()
                && session.read(cx).wants_drm()
            {
                this.ask(cx);
            }
        })
        .detach();

        let mut drm = Self {
            state: CdmState::Missing,
            offer: None,
            session,
            io,
            task: None,
        };
        if drm::supported() {
            drm.look(cx);
        }
        drm
    }

    pub fn state(&self) -> &CdmState {
        &self.state
    }

    /// Whether this build has a host for a module at all. False means the platform has no
    /// host yet, so nothing about a module is worth showing the user.
    pub fn supported(&self) -> bool {
        drm::supported()
    }

    /// Answers from what is already here, off the network. With nothing here and a protected
    /// account present, the user is asked; with a module Sonora fetched before, a newer one is
    /// fetched quietly.
    pub fn look(&mut self, cx: &mut Context<Self>) {
        self.state = CdmState::Looking;
        cx.notify();

        let io = self.io.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let found = io.spawn_blocking(drm::find).await.ok().flatten();
            this.update(cx, |this, cx| {
                this.task = None;
                this.state = match &found {
                    Some(found) => CdmState::Ready(found.origin),
                    None => CdmState::Missing,
                };
                cx.notify();
                if !this.session.read(cx).wants_drm() {
                    return;
                }
                match &found {
                    None => this.ask(cx),
                    Some(found) if found.origin == Origin::Fetched => this.refresh(cx),
                    Some(_) => {}
                }
            })
            .ok();
        }));
    }

    /// Puts the question up, unless one is already up, the user answered it this run, or a
    /// module is here.
    fn ask(&mut self, cx: &mut Context<Self>) {
        if self.state != CdmState::Missing {
            return;
        }
        self.state = CdmState::Wanted;
        cx.notify();
    }

    /// The user's no, to either question. A download under way is dropped.
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.task = None;
        self.offer = None;
        self.state = CdmState::Declined;
        cx.notify();
    }

    /// The user's yes to downloading: fetches the archive and puts the terms up.
    pub fn download(&mut self, cx: &mut Context<Self>) {
        self.state = CdmState::Offering;
        cx.notify();

        let io = self.io.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let offered = io.spawn(drm::offer()).await;
            this.update(cx, |this, cx| {
                this.task = None;
                match offered {
                    Ok(Ok(offer)) => {
                        this.state = CdmState::Offered(Terms {
                            version: offer.version().to_string(),
                            license: offer.license().to_string(),
                        });
                        this.offer = Some(offer);
                    }
                    Ok(Err(error)) => {
                        log::warn!("drm: cannot download the widevine module: {error:#}");
                        this.state = CdmState::Missing;
                    }
                    Err(_) => this.state = CdmState::Missing,
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The user's yes to the terms: installs what was downloaded.
    pub fn accept(&mut self, cx: &mut Context<Self>) {
        let Some(offer) = self.offer.take() else {
            return;
        };
        self.state = CdmState::Installing;
        cx.notify();

        let io = self.io.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let installed = io.spawn_blocking(move || offer.install()).await;
            this.update(cx, |this, cx| {
                this.task = None;
                this.state = match installed {
                    Ok(Ok(found)) => CdmState::Ready(found.origin),
                    Ok(Err(error)) => {
                        log::warn!("drm: cannot install the widevine module: {error:#}");
                        CdmState::Missing
                    }
                    Err(_) => CdmState::Missing,
                };
                cx.notify();
            })
            .ok();
        }));
    }

    /// Removes the module Sonora fetched from Google and settles on whatever else is here, a
    /// browser's copy or nothing. Nothing is asked again this run.
    pub fn uninstall(&mut self, cx: &mut Context<Self>) {
        self.offer = None;
        self.state = CdmState::Looking;
        cx.notify();

        let io = self.io.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let found = io
                .spawn_blocking(|| {
                    drm::uninstall()?;
                    anyhow::Ok(drm::find())
                })
                .await;
            this.update(cx, |this, cx| {
                this.task = None;
                this.state = match found {
                    Ok(Ok(Some(found))) => CdmState::Ready(found.origin),
                    Ok(Ok(None)) => CdmState::Declined,
                    Ok(Err(error)) => {
                        log::warn!("drm: cannot remove the widevine module: {error:#}");
                        match drm::find() {
                            Some(found) => CdmState::Ready(found.origin),
                            None => CdmState::Declined,
                        }
                    }
                    Err(_) => CdmState::Declined,
                };
                cx.notify();
            })
            .ok();
        }));
    }

    /// Fetches a newer version of a module the user accepted before, without asking. The one
    /// already open stays in use until the next run.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let io = self.io.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let fetched = io.spawn(drm::fetch()).await;
            this.update(cx, |this, _| {
                this.task = None;
                if let Ok(Err(error)) = fetched {
                    log::warn!("drm: cannot refresh the widevine module: {error:#}");
                }
            })
            .ok();
        }));
    }
}
