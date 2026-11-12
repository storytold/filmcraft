//! CELT (RFC 6716 §4.3) pieces the encoder needs: the static mode, band layout and allocation,
//! energy and band-shape coding, and the MDCT.

pub(crate) mod bands;
pub(crate) mod encode;
pub(crate) mod energy;
pub(crate) mod mdct;
pub(crate) mod rate;
pub(crate) mod tables;

use std::sync::OnceLock;

use rate::Mode;

pub(crate) fn mode() -> &'static Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    MODE.get_or_init(Mode::new)
}
