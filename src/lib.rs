//! Camera-consistent spatial resizing and portable image conversion for the `rro` executable.
//!
//! [`core`] operates on floating-point image channels without color conversion or
//! integer clipping. [`codec`] reads and writes image files as straight-alpha,
//! 8-bit sRGB pixels while recording source alpha-channel presence. The executable
//! filters RGB directly or premultiplies alpha-bearing images, then publishes output
//! atomically.
//!
//! The numerical implementation preserves the centered grid and camera-derived
//! kernels of the pinned `ResizeRightOptics` reference. It does not expose the
//! original tensor, differentiation, or research interfaces.
//!
//! # Examples
//!
//! ```
//! use rro::core::{self, Image, ResizeOptions};
//!
//! # fn main() -> Result<(), rro::core::ResizeError> {
//! let input = Image {
//!     width: 3,
//!     height: 2,
//!     channels: 1,
//!     data: vec![0.5; 6],
//! };
//! let options = ResizeOptions {
//!     scale_x: Some(0.5),
//!     scale_y: Some(0.5),
//!     ..ResizeOptions::default()
//! };
//! let output = core::resize(&input, &options)?;
//! assert_eq!((output.width, output.height), (2, 1), "scale uses a guarded ceiling");
//! # Ok(())
//! # }
//! ```
pub mod codec;
pub mod core;

#[cfg(test)]
mod tests {
    use super::{codec, core};

    #[test]
    fn exported_types_preserve_thread_and_error_contracts() {
        fn assert_send_sync<T: Send + Sync>() {}
        fn assert_error<T: std::error::Error + Send + Sync + 'static>() {}

        assert_send_sync::<core::Image>();
        assert_send_sync::<core::ResizeOptions>();
        assert_send_sync::<codec::RgbaImage>();
        assert_error::<core::ResizeError>();
        assert_error::<codec::CodecError>();
    }
}
