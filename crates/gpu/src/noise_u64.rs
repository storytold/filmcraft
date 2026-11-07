//! The u64 emulation of the value-noise hash in WGSL (`noise_u64.wgsl`), checked without a GPU.

/// WGSL source of the u64-pair hash (`hash_u64`), mirrored by `filmcraft_render`'s test module.
#[cfg(test)]
pub(crate) const NOISE_U64_WGSL: &str = include_str!("noise_u64.wgsl");

#[cfg(test)]
mod tests {
    use super::NOISE_U64_WGSL;

    /// The shader parses and validates on its own (entry point added so nothing is dead code).
    #[test]
    fn noise_u64_wgsl_validates() {
        let src = format!(
            "{NOISE_U64_WGSL}\n@group(0) @binding(0) var<storage, read_write> out: array<u32, 2>;\n\
             @compute @workgroup_size(1) fn main() {{\n\
             let h = hash_u64(u64_of_i32(1), u64_of_i32(-2), u64_of_i32(3), U64(0u, 42u));\n\
             out[0] = h.hi;\n out[1] = h.lo;\n}}\n"
        );
        let module = naga::front::wgsl::parse_str(&src).expect("noise_u64.wgsl parses");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .expect("noise_u64.wgsl validates");
    }

    /// Every hash constant in the WGSL is the CPU's `vfx::hash_u` constant, split into halves. A wrong
    /// digit only shows on the GPU, so this runs without one. Regression: the z and seed multipliers
    /// had a digit wrong (0x166567B1 / 0x166567C5 instead of 0x165667B1 / 0x165667C5).
    #[test]
    fn noise_u64_constants_match_cpu_hash() {
        let consts =
            [0x9E37_79B9_7F4A_7C15u64, 0xC2B2_AE3D_27D4_EB4F, 0x1656_67B1_9E37_79F9, 0x27D4_EB2F_1656_67C5, 0xBF58_476D_1CE4_E5B9, 0x94D0_49BB_1331_11EB];
        for c in consts {
            let pair = format!("U64(0x{:08X}u, 0x{:08X}u)", c >> 32, c & 0xffff_ffff);
            assert!(NOISE_U64_WGSL.contains(&pair), "noise_u64.wgsl lacks the CPU constant {pair}");
        }
    }

    /// Validates one kernel module: `source` (with its own bindings) must parse and validate.
    fn validate(source: &str) -> Result<(), String> {
        let module = naga::front::wgsl::parse_str(source).map_err(|e| e.emit_to_string(source))?;
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .map(|_| ())
            .map_err(|e| format!("{e:?}"))
    }

    /// The spacer kernel validates with the shared contract bindings (0-3).
    #[test]
    fn spacer_wgsl_validates() {
        validate(include_str!("fx_spacer.wgsl")).expect("fx_spacer.wgsl validates");
    }

    /// The roughen kernels validate together with the u64 hash they call.
    #[test]
    fn roughen_wgsl_validates() {
        let src = format!("{NOISE_U64_WGSL}\n{}", include_str!("fx_roughen.wgsl"));
        validate(&src).expect("noise_u64.wgsl + fx_roughen.wgsl validates");
    }
}
