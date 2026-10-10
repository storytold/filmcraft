//! Windows graphics set-up, applied before wgpu can initialize any driver: which backends the
//! instance creates, and which adapter the window draws with.
//!
//! **Backends.** eframe's default instance includes OpenGL; creating it can crash inside AMD's
//! `atio6axx.dll` before any Rust code runs, so the instance is limited to Vulkan and DirectX 12
//! (see [`configure`]).
//!
//! **Adapter.** A machine with two driver packages for one GPU (a laptop whose integrated and
//! discrete AMD GPUs were installed from different driver versions, each with its own Vulkan ICD)
//! lists that GPU twice: once through each `amdvlk64.dll`. wgpu's default picks the first discrete
//! adapter, which is whichever ICD the loader found first; drawing through the stale one kills the
//! process with an access violation inside the driver (0xc0000005, no Rust frame to catch it). The
//! selector installed here keeps wgpu's order between different GPUs but, among entries that are the
//! same GPU, prefers the driver that exposes the most features: the newer one. Nothing here is tied
//! to a GPU, driver or machine: with one driver per GPU the rule has nothing to merge and the choice
//! is wgpu's own. `WGPU_ADAPTER_NAME` (a case-insensitive part of the adapter's name, backend or
//! driver as logged) picks an adapter by hand, as in wgpu's examples.

use std::cmp::Reverse;
use std::fmt;
use std::sync::Arc;

use eframe::wgpu::{Adapter, Backend, Backends, DeviceType, PowerPreference, Surface};
use eframe::{NativeOptions, egui_wgpu::WgpuSetup};

/// Environment variable naming the adapter to draw with: a case-insensitive part of its name,
/// backend (`Vulkan`, `Dx12`) or driver text, so that two entries of one GPU can be told apart.
/// Unset, empty or matching no adapter that can draw to the window: the automatic choice.
pub const ADAPTER_NAME_VAR: &str = "WGPU_ADAPTER_NAME";

/// Restrict the wgpu instance to the primary backends (Vulkan, DirectX 12, Metal), i.e. leave
/// OpenGL out, unless `backend_override` (normally `Backends::from_env()`, i.e. `WGPU_BACKEND`)
/// names other backends; and choose the adapter with [`choose`].
pub fn configure(options: &mut NativeOptions, backend_override: Option<Backends>) {
    if let WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup {
        // eframe's default includes GL. Creating that backend can crash inside AMD's
        // atio6axx.dll before adapter selection or any Rust error handling runs, so the
        // window never appears. Choosing an adapter afterwards is too late: only leaving GL
        // out of the instance avoids it. Keep Vulkan and DX12 both: forcing DX12 alone sends
        // shaders through FXC (which rejects fx.wgsl, silently falling back to the CPU) and
        // leaves machines without DX12 with no backend at all.
        create.instance_descriptor.backends = backend_override.unwrap_or(Backends::PRIMARY);
        // `WGPU_POWER_PREF` still applies: egui-wgpu read it into `power_preference`, which wgpu
        // would ignore once a selector is set.
        let power = create.power_preference;
        let wanted = std::env::var(ADAPTER_NAME_VAR).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
        create.native_adapter_selector =
            Some(Arc::new(move |adapters: &[Adapter], surface: Option<&Surface<'_>>| select(adapters, surface, power, wanted.as_deref())));
    }
}

/// One adapter's identity and capabilities, apart from the `wgpu::Adapter` so that the choice is a
/// pure function ([`choose`]) tests exercise without a GPU.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub backend: Backend,
    pub device_type: DeviceType,
    /// PCI vendor and device ids; both zero when the backend does not report them (OpenGL).
    pub vendor: u32,
    pub device: u32,
    /// PCI bus id such as `0000:03:00.0`, or empty when the driver does not report one.
    pub pci_bus_id: String,
    /// Driver name and version, for the log.
    pub driver: String,
    /// How many wgpu features the driver exposes; a newer driver of the same GPU exposes more.
    pub features: usize,
    pub max_texture_dimension_2d: u32,
    pub max_buffer_size: u64,
    /// Whether the window surface can be drawn through this adapter (true without a surface).
    pub draws_to_window: bool,
}

impl Candidate {
    fn describe(adapter: &Adapter, surface: Option<&Surface<'_>>) -> Candidate {
        let info = adapter.get_info();
        let limits = adapter.limits();
        Candidate {
            driver: format!("{} {}", info.driver, info.driver_info).trim().to_owned(),
            name: info.name,
            backend: info.backend,
            device_type: info.device_type,
            vendor: info.vendor,
            device: info.device,
            pci_bus_id: info.device_pci_bus_id,
            features: adapter.features().iter().count(),
            max_texture_dimension_2d: limits.max_texture_dimension_2d,
            max_buffer_size: limits.max_buffer_size,
            draws_to_window: surface.is_none_or(|s| adapter.is_surface_supported(s)),
        }
    }

    /// Whether `wanted` ([`ADAPTER_NAME_VAR`], already trimmed and lower-cased) names this adapter:
    /// part of its name, backend or driver text.
    fn named(&self, wanted: &str) -> bool {
        [self.name.as_str(), &format!("{:?}", self.backend), self.driver.as_str()].iter().any(|text| text.to_lowercase().contains(wanted))
    }
}

impl fmt::Display for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.device_type {
            DeviceType::DiscreteGpu => "discrete GPU",
            DeviceType::IntegratedGpu => "integrated GPU",
            DeviceType::VirtualGpu => "virtual GPU",
            DeviceType::Cpu => "software",
            DeviceType::Other => "unknown type",
        };
        write!(f, "\"{}\" ({:?}, {kind}, {}, {} features", self.name, self.backend, self.driver, self.features)?;
        if !self.pci_bus_id.is_empty() {
            write!(f, ", bus {}", self.pci_bus_id)?;
        }
        if !self.draws_to_window {
            write!(f, ", cannot draw to the window")?;
        }
        write!(f, ")")
    }
}

/// Whether two entries are one physical GPU seen through two drivers: same backend, PCI vendor and
/// device, and the same bus id when both drivers report one. Adapters without ids (OpenGL reports
/// zeros) are never duplicates: two of them cannot be told apart.
fn same_gpu(a: &Candidate, b: &Candidate) -> bool {
    let identified = a.vendor != 0 || a.device != 0;
    let same_bus = a.pci_bus_id.is_empty() || b.pci_bus_id.is_empty() || a.pci_bus_id == b.pci_bus_id;
    identified && a.backend == b.backend && a.vendor == b.vendor && a.device == b.device && same_bus
}

/// wgpu's own device-type preference for `power`: lower sorts first; `None` keeps enumeration order.
fn type_order(device_type: DeviceType, power: PowerPreference) -> u8 {
    let prefer_integrated = match power {
        PowerPreference::LowPower => true,
        PowerPreference::HighPerformance => false,
        PowerPreference::None => return 0,
    };
    match device_type {
        DeviceType::DiscreteGpu if prefer_integrated => 2,
        DeviceType::IntegratedGpu if prefer_integrated => 1,
        DeviceType::DiscreteGpu => 1,
        DeviceType::IntegratedGpu => 2,
        DeviceType::Other => 3,
        DeviceType::VirtualGpu => 4,
        DeviceType::Cpu => 5,
    }
}

/// Sort key of `candidate` (at `index` in `candidates`): wgpu's order (device type for `power`,
/// then enumeration order) between different GPUs, and among the entries of one GPU the driver
/// exposing the most features, then the highest limits. Entries of one GPU share the position of
/// the first of them, so a duplicate never jumps ahead of a different GPU wgpu would have preferred.
fn rank(candidates: &[Candidate], index: usize, candidate: &Candidate, power: PowerPreference) -> (u8, usize, Reverse<(usize, u32, u64)>, usize) {
    let first_entry = candidates.iter().position(|other| same_gpu(candidate, other)).unwrap_or(index);
    (type_order(candidate.device_type, power), first_entry, Reverse((candidate.features, candidate.max_texture_dimension_2d, candidate.max_buffer_size)), index)
}

/// The index of the adapter to draw with, among those that can draw to the window: the one
/// `wanted_name` names (part of the name, backend or driver, case-insensitive) when any does, else the best by
/// [`rank`]. `None` when no adapter can draw to the window.
pub fn choose(candidates: &[Candidate], power: PowerPreference, wanted_name: Option<&str>) -> Option<usize> {
    let best = |mut usable: Vec<(usize, &Candidate)>| {
        usable.sort_by_key(|&(index, candidate)| rank(candidates, index, candidate, power));
        usable.first().map(|&(index, _)| index)
    };
    let usable: Vec<(usize, &Candidate)> = candidates.iter().enumerate().filter(|(_, c)| c.draws_to_window).collect();
    if let Some(wanted) = wanted_name.map(str::trim).filter(|w| !w.is_empty()).map(str::to_lowercase) {
        let named: Vec<(usize, &Candidate)> = usable.iter().copied().filter(|(_, c)| c.named(&wanted)).collect();
        if let Some(index) = best(named) {
            return Some(index);
        }
    }
    best(usable)
}

/// One GPU listed more than once through one backend: one driver package per entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Duplicate {
    pub backend: Backend,
    pub name: String,
    /// The driver of each entry, in enumeration order.
    pub drivers: Vec<String>,
}

/// GPUs listed more than once (see [`same_gpu`]), in enumeration order.
pub fn duplicates(candidates: &[Candidate]) -> Vec<Duplicate> {
    let mut grouped = vec![false; candidates.len()];
    let mut out = Vec::new();
    for (i, candidate) in candidates.iter().enumerate() {
        if grouped.get(i).copied().unwrap_or(true) {
            continue;
        }
        let mut drivers = Vec::new();
        for (j, other) in candidates.iter().enumerate().skip(i) {
            if same_gpu(candidate, other) {
                drivers.push(other.driver.clone());
                if let Some(g) = grouped.get_mut(j) {
                    *g = true;
                }
            }
        }
        if drivers.len() > 1 {
            out.push(Duplicate { backend: candidate.backend, name: candidate.name.clone(), drivers });
        }
    }
    out
}

/// egui-wgpu's `native_adapter_selector`: [`choose`] over the enumerated adapters, with the
/// candidates, duplicates and the choice in the log (`<data dir>/Logs/filmcraft.log`).
fn select(adapters: &[Adapter], surface: Option<&Surface<'_>>, power: PowerPreference, wanted_name: Option<&str>) -> Result<Adapter, String> {
    let candidates: Vec<Candidate> = adapters.iter().map(|a| Candidate::describe(a, surface)).collect();
    for candidate in &candidates {
        log::info!("graphics adapter available: {candidate}");
    }
    for Duplicate { backend, name, drivers } in duplicates(&candidates) {
        log::warn!(
            "{backend:?} lists \"{name}\" {} times ({}): one GPU with several driver packages installed; \
             using the entry with the most features. Installing one driver package for every GPU removes the duplicate.",
            drivers.len(),
            drivers.join(" / ")
        );
    }
    let index = choose(&candidates, power, wanted_name).ok_or_else(|| {
        let listed: Vec<String> = candidates.iter().map(ToString::to_string).collect();
        format!("no graphics adapter can draw to the window (found {}: {})", listed.len(), listed.join(", "))
    })?;
    let (Some(chosen), Some(adapter)) = (candidates.get(index), adapters.get(index)) else {
        return Err(format!("graphics adapter {index} of {} disappeared while choosing", adapters.len()));
    };
    if let Some(wanted) = wanted_name
        && !chosen.named(&wanted.trim().to_lowercase())
    {
        log::warn!("{ADAPTER_NAME_VAR}={wanted:?} names no adapter that can draw to the window; chosen automatically instead");
    }
    log::info!("graphics adapter chosen: {chosen} ({power:?})");
    Ok(adapter.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured(backend_override: Option<Backends>) -> eframe::egui_wgpu::WgpuSetupCreateNew {
        let mut options = NativeOptions::default();
        configure(&mut options, backend_override);
        let WgpuSetup::CreateNew(create) = options.wgpu_options.wgpu_setup else {
            panic!("default native options must create a wgpu instance");
        };
        create
    }

    #[test]
    fn windows_default_leaves_out_gl() {
        // eframe's own default (PRIMARY | GL) would initialize OpenGL as well.
        let WgpuSetup::CreateNew(eframe_default) = NativeOptions::default().wgpu_options.wgpu_setup else {
            panic!("default native options must create a wgpu instance");
        };
        assert!(eframe_default.instance_descriptor.backends.contains(Backends::GL));
        let configured = configured(None).instance_descriptor.backends;
        assert_eq!(configured, Backends::PRIMARY);
        assert!(!configured.contains(Backends::GL));
        assert!(configured.contains(Backends::DX12) && configured.contains(Backends::VULKAN));
    }

    #[test]
    fn explicit_backend_override_is_preserved() {
        for backends in [Backends::VULKAN, Backends::GL, Backends::DX12, Backends::VULKAN | Backends::DX12, Backends::empty()] {
            assert_eq!(configured(Some(backends)).instance_descriptor.backends, backends);
        }
    }

    #[test]
    fn the_adapter_selector_is_installed() {
        assert!(configured(None).native_adapter_selector.is_some());
        assert!(configured(Some(Backends::DX12)).native_adapter_selector.is_some());
    }

    fn gpu(name: &str, backend: Backend, device_type: DeviceType, device: u32, bus: &str, driver: &str, features: usize) -> Candidate {
        Candidate {
            name: name.into(),
            backend,
            device_type,
            vendor: 0x1002,
            device,
            pci_bus_id: bus.into(),
            driver: driver.into(),
            features,
            max_texture_dimension_2d: 16384,
            max_buffer_size: 1 << 32,
            draws_to_window: true,
        }
    }

    const RX_6800M: u32 = 0x73df;
    const RENOIR_IGPU: u32 = 0x1638;

    /// An ASUS G513QY as `vulkaninfo` lists it when the integrated and the discrete GPU were
    /// installed from different driver packages: the RX 6800M through the 2021 ICD (Vulkan 1.2,
    /// first), then through the 2026 ICD (Vulkan 1.4), then the integrated GPU; DirectX 12 after
    /// Vulkan, as wgpu enumerates. wgpu's default (the first discrete adapter) is entry 0, and
    /// drawing through it crashes inside the driver.
    fn g513qy() -> Vec<Candidate> {
        vec![
            gpu("AMD Radeon RX 6800M", Backend::Vulkan, DeviceType::DiscreteGpu, RX_6800M, "0000:03:00.0", "AMD proprietary driver 26.9.2", 49),
            gpu(
                "AMD Radeon RX 6800M",
                Backend::Vulkan,
                DeviceType::DiscreteGpu,
                RX_6800M,
                "0000:03:00.0",
                "AMD proprietary driver 26.9.2 (AMD proprietary shader compiler)",
                63,
            ),
            gpu("AMD Radeon(TM) Graphics", Backend::Vulkan, DeviceType::IntegratedGpu, RENOIR_IGPU, "0000:08:00.0", "AMD proprietary driver 21.30.02.13", 47),
            gpu("AMD Radeon RX 6800M", Backend::Dx12, DeviceType::DiscreteGpu, RX_6800M, "0000:03:00.0", "32.0.21045.11001", 52),
            gpu("AMD Radeon(TM) Graphics", Backend::Dx12, DeviceType::IntegratedGpu, RENOIR_IGPU, "0000:08:00.0", "30.0.13002.13003", 50),
        ]
    }

    #[test]
    fn one_gpu_listed_through_two_drivers_draws_with_the_driver_exposing_more_features() {
        let adapters = g513qy();
        assert_eq!(choose(&adapters, PowerPreference::HighPerformance, None), Some(1));
        // The same list with the entries swapped still picks the richer driver.
        let mut swapped = adapters.clone();
        swapped.swap(0, 1);
        assert_eq!(choose(&swapped, PowerPreference::HighPerformance, None), Some(0));
        // Equal drivers: wgpu's order, the first entry.
        let mut equal = adapters.clone();
        if let Some(c) = equal.get_mut(1) {
            c.features = 49;
        }
        assert_eq!(choose(&equal, PowerPreference::HighPerformance, None), Some(0));
        // Equal features: the higher limits decide.
        if let Some(c) = equal.get_mut(1) {
            c.max_texture_dimension_2d = 32768;
        }
        assert_eq!(choose(&equal, PowerPreference::HighPerformance, None), Some(1));
    }

    #[test]
    fn duplicates_are_reported_per_gpu() {
        let dup = duplicates(&g513qy());
        assert_eq!(dup.len(), 1, "{dup:?}");
        assert_eq!((dup[0].backend, dup[0].name.as_str()), (Backend::Vulkan, "AMD Radeon RX 6800M"));
        assert_eq!(dup[0].drivers.len(), 2);
        assert!(dup[0].drivers[1].contains("shader compiler"));
        assert!(duplicates(&g513qy()[2..]).is_empty());
    }

    #[test]
    fn a_duplicate_never_overtakes_a_gpu_wgpu_would_prefer() {
        // Low power: the integrated GPU comes first even though a discrete duplicate has more features.
        assert_eq!(choose(&g513qy(), PowerPreference::LowPower, None), Some(2));
        // No preference: enumeration order between GPUs, the richer driver within the GPU. Rotated so the
        // integrated GPU and the DirectX entries come first, the DirectX 6800M is the first discrete adapter
        // and the Vulkan duplicates behind it stay behind it.
        assert_eq!(choose(&g513qy(), PowerPreference::None, None), Some(1));
        let mut igpu_first = g513qy();
        igpu_first.rotate_left(2);
        assert_eq!(choose(&igpu_first, PowerPreference::None, None), Some(0));
        assert_eq!(choose(&igpu_first, PowerPreference::HighPerformance, None), Some(1));
    }

    #[test]
    fn without_duplicates_the_choice_is_wgpus() {
        let adapters = vec![
            gpu("AMD Radeon(TM) Graphics", Backend::Vulkan, DeviceType::IntegratedGpu, RENOIR_IGPU, "", "x", 60),
            gpu("AMD Radeon RX 6800M", Backend::Vulkan, DeviceType::DiscreteGpu, RX_6800M, "", "x", 40),
            gpu("AMD Radeon RX 6800M", Backend::Dx12, DeviceType::DiscreteGpu, RX_6800M, "", "x", 70),
            gpu("Microsoft Basic Render Driver", Backend::Dx12, DeviceType::Cpu, 0x8c, "", "x", 80),
        ];
        assert_eq!(choose(&adapters, PowerPreference::HighPerformance, None), Some(1));
        assert_eq!(choose(&adapters, PowerPreference::LowPower, None), Some(0));
        assert_eq!(choose(&adapters, PowerPreference::None, None), Some(0));
        let software_only: Vec<Candidate> = adapters[3..].to_vec();
        assert_eq!(choose(&software_only, PowerPreference::HighPerformance, None), Some(0));
    }

    #[test]
    fn two_cards_of_one_model_are_not_duplicates() {
        let two_cards = vec![
            gpu("AMD Radeon RX 6800M", Backend::Vulkan, DeviceType::DiscreteGpu, RX_6800M, "0000:03:00.0", "x", 49),
            gpu("AMD Radeon RX 6800M", Backend::Vulkan, DeviceType::DiscreteGpu, RX_6800M, "0000:04:00.0", "x", 63),
        ];
        assert!(duplicates(&two_cards).is_empty());
        assert_eq!(choose(&two_cards, PowerPreference::HighPerformance, None), Some(0));
        // A driver that reports no bus id still counts as the same GPU as one that does.
        let one_unreported = vec![
            gpu("AMD Radeon RX 6800M", Backend::Vulkan, DeviceType::DiscreteGpu, RX_6800M, "", "old", 49),
            gpu("AMD Radeon RX 6800M", Backend::Vulkan, DeviceType::DiscreteGpu, RX_6800M, "0000:03:00.0", "new", 63),
        ];
        assert_eq!(duplicates(&one_unreported).len(), 1);
        assert_eq!(choose(&one_unreported, PowerPreference::HighPerformance, None), Some(1));
        // Adapters without PCI ids (OpenGL) cannot be told apart, so they are never merged.
        let mut gl = two_cards.clone();
        for c in &mut gl {
            c.backend = Backend::Gl;
            c.vendor = 0;
            c.device = 0;
            c.pci_bus_id.clear();
        }
        assert!(duplicates(&gl).is_empty());
        assert_eq!(choose(&gl, PowerPreference::HighPerformance, None), Some(0));
    }

    #[test]
    fn adapters_that_cannot_draw_to_the_window_are_skipped() {
        let mut adapters = g513qy();
        for c in adapters.iter_mut().filter(|c| c.backend == Backend::Vulkan) {
            c.draws_to_window = false;
        }
        assert_eq!(choose(&adapters, PowerPreference::HighPerformance, None), Some(3));
        for c in &mut adapters {
            c.draws_to_window = false;
        }
        assert_eq!(choose(&adapters, PowerPreference::HighPerformance, None), None);
        assert_eq!(choose(&[], PowerPreference::HighPerformance, None), None);
    }

    #[test]
    fn an_adapter_named_in_the_environment_wins() {
        let adapters = g513qy();
        assert_eq!(choose(&adapters, PowerPreference::HighPerformance, Some("radeon(tm) graphics")), Some(2));
        // Several entries match: the best of them by the usual rule.
        assert_eq!(choose(&adapters, PowerPreference::HighPerformance, Some("6800M")), Some(1));
        assert_eq!(choose(&adapters, PowerPreference::LowPower, Some("AMD")), Some(2));
        // Two entries of one GPU share a name; the backend or the driver text tells them apart.
        assert_eq!(choose(&adapters, PowerPreference::HighPerformance, Some("dx12")), Some(3));
        assert_eq!(choose(&adapters, PowerPreference::HighPerformance, Some("shader compiler")), Some(1));
        assert_eq!(choose(&adapters, PowerPreference::HighPerformance, Some("32.0.21045")), Some(3));
        let old_driver_only: Vec<Candidate> = adapters.iter().filter(|c| c.features != 63).cloned().collect();
        assert_eq!(choose(&old_driver_only, PowerPreference::HighPerformance, Some("Vulkan")), Some(0));
        // Blank or unknown names fall back to the automatic choice.
        assert_eq!(choose(&adapters, PowerPreference::HighPerformance, Some("  ")), Some(1));
        assert_eq!(choose(&adapters, PowerPreference::HighPerformance, Some("GeForce")), Some(1));
        // A named adapter that cannot draw to the window is not chosen.
        let mut no_igpu = adapters.clone();
        for c in no_igpu.iter_mut().filter(|c| c.device == RENOIR_IGPU) {
            c.draws_to_window = false;
        }
        assert_eq!(choose(&no_igpu, PowerPreference::HighPerformance, Some("radeon(tm) graphics")), Some(1));
    }

    #[test]
    fn candidates_describe_themselves_for_the_log() {
        let adapters = g513qy();
        let text = adapters[1].to_string();
        assert!(text.starts_with("\"AMD Radeon RX 6800M\" (Vulkan, discrete GPU, AMD proprietary driver 26.9.2"), "{text}");
        assert!(text.contains("63 features") && text.contains("bus 0000:03:00.0") && text.ends_with(')'), "{text}");
        let mut offscreen = adapters[4].clone();
        offscreen.draws_to_window = false;
        offscreen.pci_bus_id.clear();
        let text = offscreen.to_string();
        assert!(text.contains("integrated GPU") && text.contains("cannot draw to the window") && !text.contains("bus"), "{text}");
    }
}
