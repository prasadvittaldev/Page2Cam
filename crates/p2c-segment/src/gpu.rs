//! Choosing the GPU for DirectML.
//!
//! DirectML's `device_id` indexes DXGI's plain adapter order, where adapter 0
//! is the GPU driving the display: on laptops the integrated one, already busy
//! with the page and the desktop. Measured on an i7-13850HX laptop: RVM took
//! ~60 ms (spikes to 680 ms) on the Intel UHD vs ~13 ms on the RTX 2000 Ada,
//! and DirectML's own "high performance" preference was ~2x slower than
//! naming the NVIDIA adapter by index. So we find the adapter ourselves.
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIFactory6, DXGI_ADAPTER_DESC1,
    DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
};

/// A hardware GPU: its index in DXGI's plain order (DirectML's `device_id`) and name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Adapter {
    pub index: u32,
    pub name: String,
}

fn name_of(desc: &DXGI_ADAPTER_DESC1) -> String {
    let len = desc
        .Description
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(desc.Description.len());
    String::from_utf16_lossy(&desc.Description[..len])
}

fn is_software(desc: &DXGI_ADAPTER_DESC1) -> bool {
    desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0
}

/// The hardware adapter Windows ranks first for high performance (e.g. the
/// NVIDIA GPU of a hybrid laptop), with its plain-order index.
pub fn high_performance_adapter() -> Option<Adapter> {
    // SAFETY: plain COM calls; every returned interface is owned and released on drop.
    unsafe {
        let f6: IDXGIFactory6 = CreateDXGIFactory1().ok()?;
        let mut want = None;
        for i in 0.. {
            let Ok(a) = f6.EnumAdapterByGpuPreference::<IDXGIAdapter1>(
                i,
                DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
            ) else {
                break;
            };
            let d = a.GetDesc1().ok()?;
            if !is_software(&d) {
                want = Some(d.AdapterLuid);
                break;
            }
        }
        let want = want?;
        let f1: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        for index in 0.. {
            let a = f1.EnumAdapters1(index).ok()?;
            let d = a.GetDesc1().ok()?;
            if d.AdapterLuid == want {
                return Some(Adapter {
                    index,
                    name: name_of(&d),
                });
            }
        }
        None
    }
}

/// Name of the adapter at `index` in DXGI's plain order.
pub fn adapter_name_at(index: u32) -> Option<String> {
    // SAFETY: as above.
    unsafe {
        let f1: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let d = f1.EnumAdapters1(index).ok()?.GetDesc1().ok()?;
        Some(name_of(&d))
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn finds_a_real_adapter_and_its_index() {
        let a = high_performance_adapter().expect("no hardware DXGI adapter found");
        assert!(!a.name.trim().is_empty());
        assert!(!a.name.contains('\0'), "{:?}", a.name);
        assert!(
            !a.name.contains("Basic Render"),
            "picked the software adapter"
        );
        // The index must point back at the same adapter in DXGI's plain order.
        assert_eq!(adapter_name_at(a.index).as_deref(), Some(a.name.as_str()));
    }
}
