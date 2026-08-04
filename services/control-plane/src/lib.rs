//! Control-plane internals that are worth reaching from outside the binary.
//!
//! Only the recommendation-model builder lives here. The scheduler, the leader lock and the
//! HTTP surface stay in `main.rs`: they are wiring, and testing them would mean standing up
//! Redis and NATS to observe a loop that does nothing but call into this module. A build, by
//! contrast, is a pure function of a catalogue and is exactly the thing that has to be verified
//! against a real one.

pub mod recsys;
