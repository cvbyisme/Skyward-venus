mod backend;
mod manager;
mod service;
#[cfg(target_os = "android")]
pub use service::run;
#[cfg(test)]
mod tests;

#[cfg(target_os = "android")]
mod probe;
#[cfg(target_os = "android")]
pub use probe::run as probe;
