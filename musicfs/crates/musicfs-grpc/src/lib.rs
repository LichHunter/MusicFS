pub mod proto {
    pub mod musicfs {
        pub mod v1 {
            tonic::include_proto!("musicfs.v1");
        }
    }
}

mod search_service;

pub use proto::musicfs::v1::music_fs_server::{MusicFs, MusicFsServer};
pub use search_service::SearchService;
