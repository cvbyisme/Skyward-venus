#![cfg_attr(not(target_os = "android"), allow(dead_code))]
mod blobs;
#[cfg(target_os = "android")]
mod client;
mod guard;
mod ipc;
mod platform_binder;
#[cfg(target_os = "android")]
mod probe;
mod protocol;
mod qservice;
pub(crate) mod root;
mod stc;
mod transport;
#[cfg(target_os = "android")]
pub(crate) use ipc::launch as ipc_launch;

#[cfg(target_os = "android")]
pub(crate) use qservice::run as qservice_run;

#[cfg(target_os = "android")]
pub(crate) use client::run as client_run;
#[cfg(target_os = "android")]
pub(crate) use probe::run as probe_run;
