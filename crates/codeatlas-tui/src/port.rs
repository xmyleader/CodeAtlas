use std::{
    error::Error,
    sync::mpsc::{Receiver, Sender, TryRecvError},
};

use codeatlas_core::{AppCommand, AppEvent};
use thiserror::Error;

/// UI-facing boundary implemented by the final application runtime.
///
/// Implementations must make [`ApplicationPort::try_recv_event`] non-blocking
/// so terminal input and rendering remain responsive.
pub trait ApplicationPort {
    type Error: Error + Send + Sync + 'static;

    /// Sends one command produced by the UI.
    ///
    /// # Errors
    ///
    /// Returns an implementation-specific transport error.
    fn send_command(&mut self, command: AppCommand) -> Result<(), Self::Error>;

    /// Tries to receive one runtime event without blocking.
    ///
    /// # Errors
    ///
    /// Returns an implementation-specific transport error.
    fn try_recv_event(&mut self) -> Result<Option<AppEvent>, Self::Error>;
}

/// Adapter for a standard-library command sender and event receiver pair.
#[derive(Debug)]
pub struct ChannelApplicationPort {
    commands: Sender<AppCommand>,
    events: Receiver<AppEvent>,
}

impl ChannelApplicationPort {
    #[must_use]
    pub const fn new(commands: Sender<AppCommand>, events: Receiver<AppEvent>) -> Self {
        Self { commands, events }
    }
}

impl ApplicationPort for ChannelApplicationPort {
    type Error = ChannelPortError;

    fn send_command(&mut self, command: AppCommand) -> Result<(), Self::Error> {
        self.commands
            .send(command)
            .map_err(|_| ChannelPortError::CommandChannelClosed)
    }

    fn try_recv_event(&mut self) -> Result<Option<AppEvent>, Self::Error> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(ChannelPortError::EventChannelClosed),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ChannelPortError {
    #[error("application command channel is closed")]
    CommandChannelClosed,
    #[error("application event channel is closed")]
    EventChannelClosed,
}
