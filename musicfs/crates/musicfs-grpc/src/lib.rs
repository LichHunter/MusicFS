pub mod proto {
    pub mod musicfs {
        pub mod v1 {
            tonic::include_proto!("musicfs.v1");
        }
    }
}

mod search_service;
mod server;
mod webhook;

pub use proto::musicfs::v1::music_fs_server::{MusicFs, MusicFsServer as MusicFsGrpcServer};
pub use proto::musicfs::v1::*;
pub use search_service::SearchService;
pub use server::MusicFsServer;
pub use webhook::{WebhookConfig, WebhookHandler, WebhookPayload};
