//! Small calloop adapters shared by nested and native backends.

use std::{
    ffi::OsStr,
    io,
    ops::Range,
    os::{fd::AsFd, unix::net::UnixStream},
};

use rustix::fd::OwnedFd;
use smithay::reexports::{
    calloop::{
        EventSource, Interest, Mode, Poll, PostAction, Readiness, Token, TokenFactory,
        generic::Generic,
    },
    wayland_server::{BindError, ListeningSocket},
};

const MAX_WAYLAND_ACCEPTS_PER_DISPATCH: usize = 16;

/// Accepts every queued Wayland client when the listening socket becomes readable.
pub struct WaylandSocketSource {
    socket: Generic<ListeningSocket>,
}

impl WaylandSocketSource {
    pub fn bind_auto(prefix: &str, range: Range<usize>) -> Result<Self, BindError> {
        Ok(Self {
            socket: Generic::new(
                ListeningSocket::bind_auto(prefix, range)?,
                Interest::READ,
                Mode::Level,
            ),
        })
    }

    pub fn socket_name(&self) -> Option<&OsStr> {
        self.socket.get_ref().socket_name()
    }
}

impl EventSource for WaylandSocketSource {
    type Event = UnixStream;
    type Metadata = ();
    type Ret = ();
    type Error = io::Error;

    fn process_events<F>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: F,
    ) -> io::Result<PostAction>
    where
        F: FnMut(Self::Event, &mut Self::Metadata),
    {
        self.socket.process_events(readiness, token, |_, socket| {
            for _ in 0..MAX_WAYLAND_ACCEPTS_PER_DISPATCH {
                let Some(client) = socket.accept()? else {
                    break;
                };
                callback(client, &mut ());
            }
            Ok(PostAction::Continue)
        })
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        tokens: &mut TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        self.socket.register(poll, tokens)
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        tokens: &mut TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        self.socket.reregister(poll, tokens)
    }

    fn unregister(&mut self, poll: &mut Poll) -> smithay::reexports::calloop::Result<()> {
        self.socket.unregister(poll)
    }
}

/// A duplicated readable fd used only as a readiness signal; the owner performs actual I/O.
pub struct ReadableFdSource {
    fd: Generic<OwnedFd>,
}

impl ReadableFdSource {
    pub fn new(fd: OwnedFd) -> Self {
        Self {
            fd: Generic::new(fd, Interest::READ, Mode::Level),
        }
    }

    pub fn duplicate(source: &impl AsFd) -> io::Result<Self> {
        Ok(Self::new(rustix::io::dup(source)?))
    }
}

impl EventSource for ReadableFdSource {
    type Event = ();
    type Metadata = ();
    type Ret = ();
    type Error = io::Error;

    fn process_events<F>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: F,
    ) -> io::Result<PostAction>
    where
        F: FnMut(Self::Event, &mut Self::Metadata),
    {
        self.fd.process_events(readiness, token, |_, _| {
            callback((), &mut ());
            Ok(PostAction::Continue)
        })
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        tokens: &mut TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        self.fd.register(poll, tokens)
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        tokens: &mut TokenFactory,
    ) -> smithay::reexports::calloop::Result<()> {
        self.fd.reregister(poll, tokens)
    }

    fn unregister(&mut self, poll: &mut Poll) -> smithay::reexports::calloop::Result<()> {
        self.fd.unregister(poll)
    }
}
