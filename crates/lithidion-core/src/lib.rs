pub mod appstore;
pub mod backend;
pub mod host;
pub mod net;
pub mod pkjs;
pub mod service;
pub mod settings;
pub mod store;
pub mod units;

pub const APP_ID: &str = "net.dunevent.Lithidion";

pub fn database_path() -> std::path::PathBuf {
    glib::user_data_dir().join("lithidion").join("lithidion.db")
}
