//! Which adapter to draw with when one GPU is listed through two versions of one driver.
//!
//! On Windows the Vulkan loader takes one driver from each display adapter's driver package. A PC
//! whose GPUs were installed from different packages of one vendor (a laptop with an integrated
//! and a discrete AMD GPU, the first on the manufacturer's old package and the second on a current
//! one) loads two versions of that driver, and the newer GPU is listed once through each. wgpu
//! takes the first entry of the device type it prefers, which is whichever version the loader
//! listed first; a window drawn through the stale one kills the process inside the driver
//! (`amdvlk64.dll`, 0xc0000005), where no Rust error handling runs.
//!
//! [`choose`] is wgpu's own rule (`Instance::request_adapter`: the adapters that can draw to the
//! surface, by device type for the power preference, then as enumerated) with one addition: when
//! the adapter that rule picks is listed again by the same driver, the entry exposing the most
//! features is used, i.e. the newer version. Without such a duplicate the choice is wgpu's own.
//! Entries of one GPU from different drivers (RADV and AMDVLK, a vendor's driver and Dozen) are
//! left alone: those are installed side by side on purpose, and wgpu's order between them stays.

use std::cmp::Reverse;
use std::fmt;

use wgpu::{Adapter, Backend, DeviceType, PowerPreference, Surface};

/// One enumerated adapter as plain data, so that [`choose`] is a pure function tests run without
/// a GPU.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub backend: Backend,
    pub device_type: DeviceType,
    /// PCI vendor and device ids; both zero when the backend reports none (OpenGL).
    pub vendor: u32,
    pub device: u32,
    /// PCI bus id such as `0000:03:00.0`; empty when the driver reports none.
    pub pci_bus_id: String,
    /// The driver's name (Vulkan) or version (DirectX 12).
    pub driver: String,
    /// More about the driver, for the log.
    pub driver_info: String,
    /// How many wgpu features the driver exposes; a newer version of one driver exposes more.
    pub features: u32,
    /// Whether the surface can be drawn through this adapter (true without a surface).
    pub usable: bool,
}

impl Candidate {
    pub fn of(adapter: &Adapter, surface: Option<&Surface<'_>>) -> Self {
        let info = adapter.get_info();
        Candidate {
            name: info.name,
            backend: info.backend,
            device_type: info.device_type,
            vendor: info.vendor,
            device: info.device,
            pci_bus_id: info.device_pci_bus_id,
            driver: info.driver,
            driver_info: info.driver_info,
            features: adapter.features().bits().0.iter().map(|word| word.count_ones()).sum(),
            usable: surface.is_none_or(|s| adapter.is_surface_supported(s)),
        }
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
        let driver = format!("{} {}", self.driver, self.driver_info);
        write!(f, "\"{}\" ({:?}, {kind}, driver {}, {} features", self.name, self.backend, driver.trim(), self.features)?;
        if !self.pci_bus_id.is_empty() {
            write!(f, ", bus {}", self.pci_bus_id)?;
        }
        if !self.usable {
            write!(f, ", cannot draw to the window")?;
        }
        write!(f, ")")
    }
}

/// Whether `a` and `b` are one GPU listed twice by one driver, i.e. through two installed
/// versions of it: the same backend, device type, PCI vendor and device and driver name, and the
/// same bus id when both report one. Adapters without PCI ids (OpenGL reports zeros) are never
/// the same: two of them cannot be told apart.
fn same_gpu_and_driver(a: &Candidate, b: &Candidate) -> bool {
    let identified = a.vendor != 0 || a.device != 0;
    let same_bus = a.pci_bus_id.is_empty() || b.pci_bus_id.is_empty() || a.pci_bus_id == b.pci_bus_id;
    identified && a.backend == b.backend && a.device_type == b.device_type && a.vendor == b.vendor && a.device == b.device && a.driver == b.driver && same_bus
}

/// wgpu's device type order for `power` (wgpu-core's `Instance::request_adapter`): lower first.
/// Without a preference the adapters stay as enumerated.
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

/// The index of the adapter wgpu's `request_adapter` picks: the first usable one of the best
/// device type for `power`. `None` without a usable adapter.
fn wgpu_choice(candidates: &[Candidate], power: PowerPreference) -> Option<usize> {
    let usable = candidates.iter().enumerate().filter(|(_, c)| c.usable);
    usable.min_by_key(|(index, c)| (type_order(c.device_type, power), *index)).map(|(index, _)| index)
}

/// A wgpu instance over the native backends (Vulkan, Metal, DX12), and OpenGL except on Windows.
/// There GL starts a WGL context thread inside the vendor driver (NVIDIA's `nvoglv64` is both its
/// GL driver and its Vulkan ICD); instances created and devices dropped concurrently then deadlock
/// in that driver (#3418). Elsewhere GL stays, for machines where it is the only working backend.
pub(crate) fn native_instance() -> wgpu::Instance {
    let backends = if cfg!(windows) { wgpu::Backends::PRIMARY } else { wgpu::Backends::PRIMARY | wgpu::Backends::GL };
    wgpu::Instance::new(wgpu::InstanceDescriptor { backends, ..wgpu::InstanceDescriptor::new_without_display_handle() })
}

/// The index of the adapter to draw with: [`wgpu_choice`], or the entry exposing the most
/// features among those that are the same GPU listed by the same driver (the first of them when
/// several expose as many). `None` without a usable adapter.
pub fn choose(candidates: &[Candidate], power: PowerPreference) -> Option<usize> {
    let first = wgpu_choice(candidates, power)?;
    let Some(preferred) = candidates.get(first) else {
        return Some(first);
    };
    let versions = candidates.iter().enumerate().filter(|(_, c)| c.usable && same_gpu_and_driver(preferred, c));
    versions.max_by_key(|(index, c)| (c.features, Reverse(*index))).map(|(index, _)| index).or(Some(first))
}

/// [`choose`] over enumerated adapters, as egui-wgpu's `native_adapter_selector` for the window
/// (`surface`). The adapters and the choice go to the log.
pub fn pick(adapters: &[Adapter], surface: Option<&Surface<'_>>, power: PowerPreference) -> Result<Adapter, String> {
    let candidates: Vec<Candidate> = adapters.iter().map(|a| Candidate::of(a, surface)).collect();
    for candidate in &candidates {
        log::info!("graphics adapter available: {candidate}");
    }
    let listed = || candidates.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
    let index = choose(&candidates, power).ok_or_else(|| format!("no usable graphics adapter among {}: {}", candidates.len(), listed()))?;
    let (Some(chosen), Some(adapter)) = (candidates.get(index), adapters.get(index)) else {
        return Err(format!("graphics adapter {index} of {} is missing", adapters.len()));
    };
    let others = candidates.iter().enumerate().filter(|(i, c)| *i != index && c.usable && same_gpu_and_driver(chosen, c)).count();
    if others > 0 {
        log::warn!(
            "{:?} lists \"{}\" {} times: several versions of its driver are installed, and a window drawn through a stale one can crash. Using the one with the most features; installing one driver package for every GPU removes the others.",
            chosen.backend,
            chosen.name,
            others.saturating_add(1)
        );
    }
    log::info!("graphics adapter chosen: {chosen}");
    Ok(adapter.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::Backend::{Dx12, Gl, Vulkan};
    use wgpu::DeviceType::{Cpu, DiscreteGpu, IntegratedGpu, Other};
    use wgpu::PowerPreference::{HighPerformance, LowPower};

    const AMD: u32 = 0x1002;
    const RX_6800M: u32 = 0x73df;
    const RADEON_IGPU: u32 = 0x1638;
    const POWERS: [PowerPreference; 3] = [PowerPreference::None, LowPower, HighPerformance];

    #[expect(clippy::too_many_arguments, reason = "one line per adapter in the listings below")]
    fn gpu(
        name: &str,
        backend: Backend,
        device_type: DeviceType,
        vendor: u32,
        device: u32,
        bus: &str,
        driver: &str,
        driver_info: &str,
        features: u32,
    ) -> Candidate {
        Candidate {
            name: name.into(),
            backend,
            device_type,
            vendor,
            device,
            pci_bus_id: bus.into(),
            driver: driver.into(),
            driver_info: driver_info.into(),
            features,
            usable: true,
        }
    }

    /// An ASUS G513QY as wgpu 30 lists it: the integrated Radeon is on the manufacturer's 2021
    /// driver package and the RX 6800M on a 2026 one, so the Vulkan loader has both versions of
    /// AMD's driver, and the 2021 one lists the RX 6800M first. A window drawn through that entry
    /// died inside `amdvlk64.dll` (0xc0000005) on every launch.
    fn g513qy() -> Vec<Candidate> {
        let amd = "AMD proprietary driver";
        vec![
            gpu("AMD Radeon RX 6800M", Vulkan, DiscreteGpu, AMD, RX_6800M, "", amd, "26.9.2", 60),
            gpu("AMD Radeon RX 6800M", Vulkan, DiscreteGpu, AMD, RX_6800M, "", amd, "26.9.2 (AMD proprietary shader compiler)", 68),
            gpu("AMD Radeon(TM) Graphics", Vulkan, IntegratedGpu, AMD, RADEON_IGPU, "", amd, "21.30.02.13", 56),
            gpu("AMD Radeon RX 6800M", Dx12, DiscreteGpu, AMD, RX_6800M, "0000:03:00.0", "32.0.21045.11001", "", 36),
            gpu("AMD Radeon(TM) Graphics", Dx12, IntegratedGpu, AMD, RADEON_IGPU, "0000:08:00.0", "30.0.13002.13003", "", 36),
            gpu("Microsoft Basic Render Driver", Dx12, Cpu, 0x1414, 0x8c, "", "10.0.26100.9549", "", 36),
        ]
    }

    #[test]
    fn a_gpu_listed_through_two_versions_of_its_driver_draws_with_the_newer_one() {
        let adapters = g513qy();
        // the window asks for high performance; `WGPU_POWER_PREF=none` for no preference
        for power in [HighPerformance, PowerPreference::None] {
            assert_eq!(wgpu_choice(&adapters, power), Some(0), "wgpu takes the stale entry, which crashed");
            assert_eq!(choose(&adapters, power), Some(1));
        }
        // whichever version the loader lists first
        let mut newer_first = adapters.clone();
        newer_first.swap(0, 1);
        assert_eq!(choose(&newer_first, HighPerformance), Some(0));
        // two entries exposing as much: wgpu's, the first
        let mut equal = adapters.clone();
        if let Some(c) = equal.get_mut(1) {
            c.features = 60;
        }
        assert_eq!(choose(&equal, HighPerformance), Some(0));
        // three versions: the one exposing the most
        let mut three = adapters.clone();
        three.insert(1, gpu("AMD Radeon RX 6800M", Vulkan, DiscreteGpu, AMD, RX_6800M, "", "AMD proprietary driver", "24.1.1", 64));
        assert_eq!(choose(&three, HighPerformance), Some(2));
    }

    #[test]
    fn a_newer_version_never_overtakes_another_gpu_or_backend() {
        // low power: the integrated GPU, as wgpu picks, whatever the discrete one is listed as
        assert_eq!(choose(&g513qy(), LowPower), Some(2));
        // DirectX 12 enumerated first: its entry of the GPU stays the choice
        let mut dx12_first = g513qy();
        dx12_first.rotate_left(3);
        for power in [HighPerformance, PowerPreference::None] {
            assert_eq!(choose(&dx12_first, power), Some(0));
        }
        assert_eq!(choose(&dx12_first, LowPower), wgpu_choice(&dx12_first, LowPower));
    }

    #[test]
    fn without_two_versions_of_one_driver_the_choice_is_wgpus() {
        let nvidia = |backend, bus, driver, features| gpu("NVIDIA GeForce RTX 4070", backend, DiscreteGpu, 0x10de, 0x2786, bus, driver, "", features);
        let intel = |backend, bus, driver, features| gpu("Intel(R) UHD Graphics 770", backend, IntegratedGpu, 0x8086, 0x4680, bus, driver, "", features);
        let radeon = |bus, driver, features| gpu("AMD Radeon RX 7900 XT", Vulkan, DiscreteGpu, AMD, 0x744c, bus, driver, "", features);
        let gl = |name: &str, features| gpu(name, Gl, Other, 0, 0, "", "", "", features);
        let without = |skip: usize| g513qy().into_iter().enumerate().filter(|(i, _)| *i != skip).map(|(_, c)| c).collect::<Vec<_>>();
        let listings: Vec<Vec<Candidate>> = vec![
            vec![],
            vec![nvidia(Vulkan, "0000:01:00.0", "NVIDIA", 70)],
            // a laptop with two GPUs, through Vulkan and DirectX 12
            vec![
                intel(Vulkan, "0000:00:02.0", "Intel Corporation", 60),
                nvidia(Vulkan, "0000:01:00.0", "NVIDIA", 70),
                nvidia(Dx12, "0000:01:00.0", "32.0.15.6094", 36),
                intel(Dx12, "0000:00:02.0", "31.0.101.5186", 36),
                gpu("Microsoft Basic Render Driver", Dx12, Cpu, 0x1414, 0x8c, "", "10.0.26100.1", "", 36),
            ],
            // one GPU through two different drivers, the second exposing more: Linux with AMDVLK and RADV
            vec![radeon("0000:03:00.0", "AMD open-source driver", 58), radeon("0000:03:00.0", "radv", 71)],
            // a vendor's driver and Dozen (Vulkan on DirectX 12)
            vec![nvidia(Vulkan, "", "Dozen", 75), nvidia(Vulkan, "0000:01:00.0", "NVIDIA", 70)],
            // two cards of one model, on their own buses
            vec![radeon("0000:03:00.0", "AMD proprietary driver", 60), radeon("0000:04:00.0", "AMD proprietary driver", 68)],
            // OpenGL reports no ids: two adapters cannot be told apart
            vec![gl("ANGLE", 20), gl("ANGLE", 30)],
            vec![gpu("llvmpipe (LLVM 19.1.0, 256 bits)", Vulkan, Cpu, 0x10005, 0, "", "llvmpipe", "", 50)],
            // the laptop above with either version of the driver alone
            without(0),
            without(1),
        ];
        for adapters in &listings {
            for power in POWERS {
                assert_eq!(choose(adapters, power), wgpu_choice(adapters, power), "{power:?} over {adapters:#?}");
            }
        }
        assert_eq!(choose(&[], HighPerformance), None);
    }

    #[test]
    fn two_entries_are_one_gpu_only_with_the_same_ids_type_driver_and_bus() {
        let stale = g513qy().swap_remove(0);
        let newer = g513qy().swap_remove(1);
        assert!(same_gpu_and_driver(&stale, &newer) && same_gpu_and_driver(&stale, &stale));
        // a bus id only one version reports does not tell them apart; two different ones do
        let on_bus = |bus: &str| Candidate { pci_bus_id: bus.into(), ..newer.clone() };
        assert!(same_gpu_and_driver(&stale, &on_bus("0000:03:00.0")));
        assert!(same_gpu_and_driver(&on_bus("0000:03:00.0"), &on_bus("0000:03:00.0")));
        assert!(!same_gpu_and_driver(&on_bus("0000:03:00.0"), &on_bus("0000:04:00.0")));
        for other in [
            Candidate { backend: Dx12, ..newer.clone() },
            Candidate { device_type: IntegratedGpu, ..newer.clone() },
            Candidate { vendor: 0x10de, ..newer.clone() },
            Candidate { device: RADEON_IGPU, ..newer.clone() },
            Candidate { driver: "radv".into(), ..newer.clone() },
        ] {
            assert!(!same_gpu_and_driver(&stale, &other), "{other}");
        }
        let unidentified = Candidate { vendor: 0, device: 0, ..newer };
        assert!(!same_gpu_and_driver(&unidentified, &unidentified));
    }

    #[test]
    fn adapters_that_cannot_draw_to_the_window_are_left_out() {
        let unusable = |which: &dyn Fn(usize, &Candidate) -> bool| {
            let mut adapters = g513qy();
            for (i, c) in adapters.iter_mut().enumerate() {
                c.usable = !which(i, c);
            }
            adapters
        };
        // the newer version cannot: the other entry is all there is of that GPU through Vulkan
        assert_eq!(choose(&unusable(&|i, _| i == 1), HighPerformance), Some(0));
        assert_eq!(choose(&unusable(&|i, _| i == 0), HighPerformance), Some(1));
        // Vulkan cannot at all: DirectX 12
        assert_eq!(choose(&unusable(&|_, c| c.backend == Vulkan), HighPerformance), Some(3));
        assert_eq!(choose(&unusable(&|_, _| true), HighPerformance), None);
    }

    #[test]
    fn candidates_describe_themselves_for_the_log() {
        let adapters = g513qy();
        let text = adapters.get(1).map(ToString::to_string).unwrap_or_default();
        assert_eq!(text, "\"AMD Radeon RX 6800M\" (Vulkan, discrete GPU, driver AMD proprietary driver 26.9.2 (AMD proprietary shader compiler), 68 features)");
        let offscreen = adapters.get(4).map(|c| Candidate { usable: false, ..c.clone() }.to_string()).unwrap_or_default();
        assert_eq!(
            offscreen,
            "\"AMD Radeon(TM) Graphics\" (Dx12, integrated GPU, driver 30.0.13002.13003, 36 features, bus 0000:08:00.0, cannot draw to the window)"
        );
    }

    /// Against the adapters of the machine running the tests: [`wgpu_choice`] is what wgpu's
    /// `request_adapter` returns, and [`pick`] returns that adapter too, or a version of it
    /// exposing more features.
    #[test]
    fn on_this_host_the_choice_is_wgpus_unless_its_driver_is_listed_twice() {
        if wgpu::Instance::enabled_backend_features().is_empty() {
            return;
        }
        // no OpenGL: the app leaves it out on Windows, where creating it can crash a driver
        let backends = wgpu::Backends::PRIMARY;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends, ..wgpu::InstanceDescriptor::new_without_display_handle() });
        let adapters = pollster::block_on(instance.enumerate_adapters(backends));
        let candidates: Vec<Candidate> = adapters.iter().map(|a| Candidate::of(a, None)).collect();
        for power in POWERS {
            let options = wgpu::RequestAdapterOptions { power_preference: power, ..Default::default() };
            let wgpu_own = pollster::block_on(instance.request_adapter(&options)).ok().map(|a| Candidate::of(&a, None));
            assert_eq!(wgpu_choice(&candidates, power).and_then(|i| candidates.get(i)), wgpu_own.as_ref(), "{power:?} over {candidates:#?}");
            let picked = pick(&adapters, None, power).ok().map(|a| Candidate::of(&a, None));
            match (&wgpu_own, &picked) {
                (Some(wgpu_own), Some(picked)) if wgpu_own != picked => {
                    assert!(same_gpu_and_driver(wgpu_own, picked) && picked.features > wgpu_own.features, "{power:?}: {picked} instead of {wgpu_own}");
                }
                _ => assert_eq!(picked, wgpu_own, "{power:?}"),
            }
        }
    }
}
