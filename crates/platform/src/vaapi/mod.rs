//! VA-API hardware decoding (Linux): FilmCraft parses the bitstream (VA-API decoding is
//! stateless) and the GPU's video engine reconstructs the pictures. Built on [`ffi`], which loads
//! libva at run time, and [`device`], the safe wrappers.

pub mod av1;
pub mod av1enc;
#[allow(unsafe_code)]
pub mod device;
#[allow(unsafe_code)]
pub mod ffi;
pub mod h264;
pub mod h264enc;
pub mod hevc;
pub mod hevcenc;
pub mod vp9;

#[cfg(test)]
mod abi_tests;

#[cfg(test)]
mod tests {
    use super::device::{Display, Session};
    use super::ffi::*;

    /// On a machine with a VA-API driver, report what it decodes; elsewhere, opening fails cleanly.
    #[test]
    fn probe_this_machine_or_fail_cleanly() {
        match Display::open_for(VAProfileH264High, VA_RT_FORMAT_YUV420) {
            Ok(d) => {
                eprintln!("VA-API {}.{} on {}", d.version.0, d.version.1, d.node);
                for (name, p) in [("H.264 High", VAProfileH264High), ("HEVC Main", VAProfileHEVCMain), ("HEVC Main10", VAProfileHEVCMain10)] {
                    eprintln!(
                        "  {name}: 8-bit {:?}, 10-bit {:?}, max {:?}",
                        d.supports(p, VA_RT_FORMAT_YUV420),
                        d.supports(p, VA_RT_FORMAT_YUV420_10),
                        d.max_size(p)
                    );
                }
                // a session can be made and dropped; bad sizes are refused, not crashed on
                let s = Session::new(d, VAProfileH264High, 8, 1920, 1088, 4).unwrap();
                assert_eq!(s.surface_count(), 4);
                drop(s);
                let d = Display::open_for(VAProfileH264High, VA_RT_FORMAT_YUV420).unwrap();
                assert!(Session::new(d, VAProfileH264High, 8, 0, 1080, 4).is_err());
                let d = Display::open_for(VAProfileH264High, VA_RT_FORMAT_YUV420).unwrap();
                assert!(Session::new(d, VAProfileH264High, 8, 1920, 1080, 0).is_err());
            }
            Err(e) => eprintln!("no VA-API here: {e}"),
        }
        assert!(Display::open("/dev/null/not-a-node").is_err());
    }
}
