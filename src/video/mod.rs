//! Video: reading camera and phone videos (Motion-JPEG AVI, MP4 with H.264 and AAC), WAV
//! sound, and H.264 + FLAC encoding into MP4 for exports from the video editor.

#[allow(dead_code)] // some helpers are only used by the host tests (tools/videotest)
pub mod aac;
pub mod avi;
#[allow(dead_code)]
pub mod bits;
pub mod flac;
#[allow(dead_code)]
pub mod h264;
pub mod h264cabac;
pub mod h264cavlc;
#[allow(dead_code)]
pub mod h264d;
pub mod mp4;
pub mod mp4read;
#[allow(dead_code)]
pub mod mp4src;
pub mod tables;
