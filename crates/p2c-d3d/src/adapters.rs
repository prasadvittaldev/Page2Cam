//! Which GPU to use: enumeration (Windows) and the pure choice logic.
//!
//! LUIDs must always come from a fresh [`enumerate`]: Chromium crashes on a
//! stale adapter LUID, so they are never persisted (the config stores a name).

/// One DXGI adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterInfo {
    /// Index in DXGI's plain `EnumAdapters1` order (DirectML's `device_id`).
    pub index: u32,
    pub name: String,
    pub vendor_id: u32,
    pub luid: u64,
    /// Position in `EnumAdapterByGpuPreference(HIGH_PERFORMANCE)` order.
    pub perf_rank: u32,
    pub software: bool,
}

/// The `gpu` setting: `auto` | `cpu` | an exact adapter name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GpuSetting {
    Auto,
    Cpu,
    Named(String),
}

impl GpuSetting {
    pub fn parse(s: &str) -> GpuSetting {
        match s.trim() {
            "auto" => GpuSetting::Auto,
            "cpu" => GpuSetting::Cpu,
            other => GpuSetting::Named(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            GpuSetting::Auto => "auto",
            GpuSetting::Cpu => "cpu",
            GpuSetting::Named(n) => n,
        }
    }
}

/// What the setting resolved to on this PC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GpuChoice {
    Gpu(AdapterInfo),
    Cpu { reason: String },
}

/// The hardware adapter Windows ranks first for high performance.
pub fn best_hardware(list: &[AdapterInfo]) -> Option<&AdapterInfo> {
    list.iter()
        .filter(|a| !a.software)
        .min_by_key(|a| (a.perf_rank, a.index))
}

/// Resolve `s` against `list`. The note, if any, is something to tell the user.
pub fn choose(list: &[AdapterInfo], s: &GpuSetting) -> (GpuChoice, Option<String>) {
    let auto = || match best_hardware(list) {
        Some(a) => GpuChoice::Gpu(a.clone()),
        None => GpuChoice::Cpu {
            reason: "no hardware GPU found".into(),
        },
    };
    match s {
        GpuSetting::Auto => (auto(), None),
        GpuSetting::Cpu => (
            GpuChoice::Cpu {
                reason: "CPU compatibility mode selected".into(),
            },
            None,
        ),
        GpuSetting::Named(n) => match list.iter().find(|a| !a.software && a.name == *n) {
            Some(a) => (GpuChoice::Gpu(a.clone()), None),
            None => (
                auto(),
                Some(format!("GPU \"{n}\" not found; using automatic choice")),
            ),
        },
    }
}

/// All DXGI adapters, software ones included (flagged). Empty off Windows.
#[cfg(not(windows))]
pub fn enumerate() -> Vec<AdapterInfo> {
    Vec::new()
}

#[cfg(windows)]
pub fn enumerate() -> Vec<AdapterInfo> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIFactory6, DXGI_ADAPTER_DESC1,
        DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
    };
    fn luid(d: &DXGI_ADAPTER_DESC1) -> u64 {
        ((d.AdapterLuid.HighPart as u32 as u64) << 32) | d.AdapterLuid.LowPart as u64
    }
    // SAFETY: plain COM calls; returned interfaces are owned and released on drop.
    unsafe {
        let Ok(f1) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return Vec::new();
        };
        // HIGH_PERFORMANCE order, matched back by LUID.
        let mut ranked = Vec::new();
        if let Ok(f6) = CreateDXGIFactory1::<IDXGIFactory6>() {
            for i in 0.. {
                let Ok(a) = f6.EnumAdapterByGpuPreference::<IDXGIAdapter1>(
                    i,
                    DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
                ) else {
                    break;
                };
                if let Ok(d) = a.GetDesc1() {
                    ranked.push(luid(&d));
                }
            }
        }
        let mut out = Vec::new();
        for index in 0.. {
            let Ok(a) = f1.EnumAdapters1(index) else {
                break;
            };
            let Ok(d) = a.GetDesc1() else { continue };
            let len = d
                .Description
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(d.Description.len());
            let l = luid(&d);
            out.push(AdapterInfo {
                index,
                name: String::from_utf16_lossy(&d.Description[..len]),
                vendor_id: d.VendorId,
                luid: l,
                perf_rank: ranked
                    .iter()
                    .position(|r| *r == l)
                    .unwrap_or(ranked.len() + index as usize) as u32,
                software: d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ad(index: u32, name: &str, perf_rank: u32, software: bool) -> AdapterInfo {
        AdapterInfo {
            index,
            name: name.into(),
            vendor_id: 0,
            luid: index as u64 + 1,
            perf_rank,
            software,
        }
    }

    fn two() -> Vec<AdapterInfo> {
        vec![ad(0, "Intel UHD", 1, false), ad(1, "NVIDIA RTX", 0, false)]
    }

    #[test]
    fn auto_prefers_best_perf_rank_hardware() {
        let (c, note) = choose(&two(), &GpuSetting::Auto);
        assert_eq!(c, GpuChoice::Gpu(two()[1].clone()));
        assert!(note.is_none());
    }

    #[test]
    fn auto_without_hardware_adapter_is_cpu() {
        let l = vec![ad(0, "Microsoft Basic Render Driver", 0, true)];
        match choose(&l, &GpuSetting::Auto).0 {
            GpuChoice::Cpu { reason } => assert!(reason.contains("no hardware GPU"), "{reason}"),
            c => panic!("{c:?}"),
        }
    }

    #[test]
    fn named_adapter_found() {
        let (c, note) = choose(&two(), &GpuSetting::Named("Intel UHD".into()));
        assert_eq!(c, GpuChoice::Gpu(two()[0].clone()));
        assert!(note.is_none());
    }

    #[test]
    fn named_adapter_missing_falls_back_to_auto_with_note() {
        let (c, note) = choose(&two(), &GpuSetting::Named("Voodoo".into()));
        assert_eq!(c, GpuChoice::Gpu(two()[1].clone()));
        assert!(note.unwrap().contains("Voodoo"));
    }

    #[test]
    fn cpu_setting_is_cpu() {
        assert!(matches!(
            choose(&two(), &GpuSetting::Cpu).0,
            GpuChoice::Cpu { .. }
        ));
    }

    #[test]
    fn setting_round_trip() {
        for s in ["auto", "cpu", "NVIDIA RTX"] {
            assert_eq!(GpuSetting::parse(s).as_str(), s);
        }
        assert_eq!(GpuSetting::parse("cpu"), GpuSetting::Cpu);
        assert_eq!(GpuSetting::parse("auto"), GpuSetting::Auto);
    }

    #[cfg(windows)]
    #[test]
    fn enumerate_lists_this_pc() {
        let l = enumerate();
        for a in &l {
            println!("{a:?}");
        }
        assert!(!l.is_empty());
        for (i, a) in l.iter().enumerate() {
            assert_eq!(a.index as usize, i);
            assert!(!a.name.trim().is_empty() && !a.name.contains('\0'));
        }
        assert!(l.iter().any(|a| !a.software));
        let (c, _) = choose(&l, &GpuSetting::Auto);
        assert!(matches!(c, GpuChoice::Gpu(_)));
    }
}
