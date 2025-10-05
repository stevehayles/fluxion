#! [doc = include_str! ("../README.md")]
#![cfg_attr(not(test), no_std)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

extern crate alloc;

pub use const_format::concatcp;
pub use fluxion_macro::{actor, generic_message, message};

mod fluxion;
pub use fluxion::*;

mod identifiers;
pub use identifiers::*;

mod actor;
pub use actor::*;

mod message;
pub use message::*;

mod references;
pub use references::*;

mod foreign;
pub use foreign::*;

pub use slacktor::Message;
