//! Library surface for the `worker` service: its configuration root, and nothing else.
//!
//! The binary is `main.rs` and everything it does lives there. What is here is the one thing an
//! outside crate has to see — `config-contract` builds this image's published contract from
//! [`config::Config`], and a contract derived from anything but the type the binary
//! deserialises is a claim about something else.

pub mod config;
