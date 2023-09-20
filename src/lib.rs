#![no_std]
#![cfg_attr(not(async_trait), feature(async_fn_in_trait))]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

extern crate alloc;

pub mod actor;

pub mod error;

pub mod message;

pub mod system;

pub mod util;

pub mod async_executors;

pub use util::{
    params::{ActorGenerics, MessageGenerics, ParamActor, SystemGenerics},
    Channel,
};
