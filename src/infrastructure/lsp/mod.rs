//! Language server processes and their transport. The protocol rules live in
//! `features::language_service`; this module only runs processes and moves bytes.
pub(crate) mod client;
#[cfg(test)]
mod client_tests;
pub(crate) mod job;
pub(crate) mod manager;
#[cfg(test)]
mod manager_tests;
