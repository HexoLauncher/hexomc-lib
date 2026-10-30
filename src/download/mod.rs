pub mod downloader;
pub mod mirror;

pub use downloader::*;
pub use mirror::{
    candidate_urls, download_source, mirror_url, set_download_source, DownloadSource,
};
