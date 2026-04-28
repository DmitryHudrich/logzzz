mod db;
mod files;
mod runner;

pub use runner::start;

#[derive(Debug, Eq, PartialEq, PartialOrd, Ord, Hash, Clone)]
pub struct FileHash(String);
