mod desktop;
#[cfg(not(dev))]
mod frontend;
mod startup;
mod tray;

pub use desktop::run;
