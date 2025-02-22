use std::{
    io::{Read, Write},
    marker::PhantomData,
    time::Duration,
};

use rmp_serde::decode;
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;

use crate::{
    host_api::{message, process},
    tag::Tag,
};

const SIGNAL: u32 = 1;
const TIMEOUT: u32 = 9027;

/// Mailbox for processes that are not linked, or linked and set to trap on notify signals.
#[derive(Debug)]
pub struct Mailbox<T: Serialize + DeserializeOwned> {
    _phantom: PhantomData<T>,
}

impl<T: Serialize + DeserializeOwned> Mailbox<T> {
    /// Create a mailbox with a specific type.
    ///
    /// ### Safety
    ///
    /// It's not safe to mix different types of mailboxes inside one process. This function should
    /// never be used directly.
    pub unsafe fn new() -> Self {
        Self {
            _phantom: PhantomData {},
        }
    }

    /// Gets next message from process' mailbox.
    ///
    /// If the mailbox is empty, this function will block until a new message arrives.
    pub fn receive(&self) -> Result<T, ReceiveError> {
        self.receive_(None, None)
    }

    /// Same as [`receive`], but only waits for the duration of timeout for the message.
    pub fn receive_timeout(&self, timeout: Duration) -> Result<T, ReceiveError> {
        self.receive_(None, Some(timeout))
    }

    /// Gets next message from process' mailbox & its tag.
    ///
    /// If the mailbox is empty, this function will block until a new message arrives.
    pub fn receive_with_tag(&self) -> Result<(T, Tag), ReceiveError> {
        let message = self.receive_(None, None)?;
        let tag = unsafe { message::get_tag() };
        Ok((message, Tag::from(tag)))
    }

    /// Gets a message with a specific tag from the mailbox.
    ///
    /// If the mailbox is empty, this function will block until a new message arrives.
    pub fn tag_receive(&self, tag: Tag) -> Result<T, ReceiveError> {
        self.receive_(Some(tag.id()), None)
    }

    /// Same as [`tag_receive`], but only waits for the duration of timeout for the tagged message.
    pub fn tag_receive_timeout(&self, tag: Tag, timeout: Duration) -> Result<T, ReceiveError> {
        self.receive_(Some(tag.id()), Some(timeout))
    }

    fn receive_(&self, tag: Option<i64>, timeout: Option<Duration>) -> Result<T, ReceiveError> {
        let tag = tag.unwrap_or(0);
        let timeout_ms = match timeout {
            // If waiting time is smaller than 1ms, round it up to 1ms.
            Some(timeout) => match timeout.as_millis() {
                0 => 1,
                other => other as u32,
            },
            None => 0,
        };
        let message_type = unsafe { message::receive(tag, timeout_ms) };
        // Mailbox can't receive Signal messages.
        assert_ne!(message_type, SIGNAL);
        // In case of timeout, return error.
        if message_type == TIMEOUT {
            return Err(ReceiveError::Timeout);
        }
        match rmp_serde::from_read(MessageRw {}) {
            Ok(result) => Ok(result),
            Err(decode_error) => Err(ReceiveError::DeserializationFailed(decode_error)),
        }
    }
}

impl<T: Serialize + DeserializeOwned> TransformMailbox<T> for Mailbox<T> {
    fn catch_link_panic(self) -> LinkMailbox<T> {
        unsafe { process::die_when_link_dies(0) };
        LinkMailbox::new()
    }
    fn panic_if_link_panics(self) -> Mailbox<T> {
        self
    }
}

/// Mailbox for linked processes.
///
/// When a process is linked to others it will also receive messages if one of the others dies.
#[derive(Debug)]
pub struct LinkMailbox<T: Serialize + DeserializeOwned> {
    _phantom: PhantomData<T>,
}

impl<T: Serialize + DeserializeOwned> LinkMailbox<T> {
    pub(crate) fn new() -> Self {
        Self {
            _phantom: PhantomData {},
        }
    }

    /// Gets next message from process' mailbox.
    ///
    /// If the mailbox is empty, this function will block until a new message arrives.
    pub fn receive(&self) -> Message<T> {
        self.receive_(None, None)
    }

    /// Same as [`receive`], but only waits for the duration of timeout for the message.
    pub fn receive_timeout(&self, timeout: Duration) -> Message<T> {
        self.receive_(None, Some(timeout))
    }

    /// Gets a message with a specific tag from the mailbox.
    ///
    /// If the mailbox is empty, this function will block until a new message arrives.
    pub fn tag_receive(&self, tag: Tag) -> Message<T> {
        self.receive_(Some(tag.id()), None)
    }

    /// Same as [`tag_receive`], but only waits for the duration of timeout for the tagged message.
    pub fn tag_receive_timeout(&self, tag: Tag, timeout: Duration) -> Message<T> {
        self.receive_(Some(tag.id()), Some(timeout))
    }

    fn receive_(&self, tag: Option<i64>, timeout: Option<Duration>) -> Message<T> {
        let tag = tag.unwrap_or(0);
        let timeout_ms = match timeout {
            // If waiting time is smaller than 1ms, round it up to 1ms.
            Some(timeout) => match timeout.as_millis() {
                0 => 1,
                other => other as u32,
            },
            None => 0,
        };
        let message_type = unsafe { message::receive(tag, timeout_ms) };

        if message_type == SIGNAL {
            let tag = unsafe { message::get_tag() };
            return Message::Signal(Tag::from(tag));
        }
        // In case of timeout, return error.
        else if message_type == TIMEOUT {
            return Message::Normal(Err(ReceiveError::Timeout));
        }

        let message = match rmp_serde::from_read(MessageRw {}) {
            Ok(result) => Ok(result),
            Err(decode_error) => Err(ReceiveError::DeserializationFailed(decode_error)),
        };
        Message::Normal(message)
    }
}

impl<T: Serialize + DeserializeOwned> TransformMailbox<T> for LinkMailbox<T> {
    fn catch_link_panic(self) -> LinkMailbox<T> {
        self
    }
    fn panic_if_link_panics(self) -> Mailbox<T> {
        unsafe { process::die_when_link_dies(1) };
        unsafe { Mailbox::new() }
    }
}

/// Represents an error while receiving a message.
#[derive(Error, Debug)]
pub enum ReceiveError {
    #[error("Deserialization failed")]
    DeserializationFailed(#[from] decode::Error),
    #[error("Timed out while waiting for message")]
    Timeout,
}

/// Returned from [`LinkMailbox::receive`] to indicate if the received message was a signal or a
/// normal message.
#[derive(Debug)]
pub enum Message<T> {
    Normal(Result<T, ReceiveError>),
    Signal(Tag),
}

impl<T> Message<T> {
    /// Returns true if received message is a signal.
    pub fn is_signal(&self) -> bool {
        match self {
            Message::Normal(_) => false,
            Message::Signal(_) => true,
        }
    }

    /// Returns the message if it's a normal one or panics if not.
    pub fn normal_or_unwrap(self) -> Result<T, ReceiveError> {
        match self {
            Message::Normal(message) => message,
            Message::Signal(_) => panic!("Message is of type Signal"),
        }
    }
}

/// A Signal that was turned into a message.
#[derive(Debug, Clone, Copy)]
pub struct Signal {}

pub trait TransformMailbox<T: Serialize + DeserializeOwned> {
    fn catch_link_panic(self) -> LinkMailbox<T>;
    fn panic_if_link_panics(self) -> Mailbox<T>;
}

// A helper struct to read and write into the message scratch buffer.
pub(crate) struct MessageRw {}
impl Read for MessageRw {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        Ok(unsafe { message::read_data(buf.as_mut_ptr(), buf.len()) })
    }
}
impl Write for MessageRw {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(unsafe { message::write_data(buf.as_ptr(), buf.len()) })
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
