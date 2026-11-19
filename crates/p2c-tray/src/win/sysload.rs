//! System load for the health icon, from Performance Data Helper counters:
//! `\Processor Information(_Total)\% Processor Utility` (busy, as Task
//! Manager shows it) and `% Processor Performance` (clock relative to base:
//! below 70 % means throttled). Only sampled while the engine is started.

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterValue,
    PdhOpenQueryW, PDH_FMT_COUNTERVALUE, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
};

use crate::health::SysLoad;

const UTILITY: &str = r"\Processor Information(_Total)\% Processor Utility";
const PERFORMANCE: &str = r"\Processor Information(_Total)\% Processor Performance";

pub struct Sampler {
    query: PDH_HQUERY,
    utility: Option<PDH_HCOUNTER>,
    performance: Option<PDH_HCOUNTER>,
    /// Rate counters need two collections before the first value.
    primed: bool,
}

// SAFETY: PDH handles are used from the one thread that owns the sampler.
unsafe impl Send for Sampler {}

fn add(query: PDH_HQUERY, path: &str) -> Option<PDH_HCOUNTER> {
    let mut c = PDH_HCOUNTER::default();
    // SAFETY: out-pointer valid; the path lives for the call.
    let r = unsafe { PdhAddEnglishCounterW(query, &HSTRING::from(path), 0, &mut c) };
    if r != 0 {
        tracing::warn!("CPU counter {path} unavailable (PDH 0x{r:08x})");
        return None;
    }
    Some(c)
}

fn value(c: Option<PDH_HCOUNTER>) -> Option<f32> {
    let c = c?;
    let mut v = PDH_FMT_COUNTERVALUE::default();
    // SAFETY: `c` belongs to an open query; out-pointer valid.
    let r = unsafe { PdhGetFormattedCounterValue(c, PDH_FMT_DOUBLE, None, &mut v) };
    // SAFETY: PDH_FMT_DOUBLE fills `doubleValue`.
    if r == 0 && v.CStatus == 0 {
        Some(unsafe { v.Anonymous.doubleValue } as f32)
    } else {
        None
    }
}

impl Sampler {
    /// `None` when PDH is not usable (no CPU reasons then).
    pub fn new() -> Option<Sampler> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: out-pointer valid.
        let r = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) };
        if r != 0 {
            tracing::warn!("cannot read CPU counters (PDH 0x{r:08x})");
            return None;
        }
        let utility = add(query, UTILITY);
        let performance = add(query, PERFORMANCE);
        Some(Sampler {
            query,
            utility,
            performance,
            primed: false,
        })
    }

    /// Collect now; values are averages since the previous call.
    pub fn sample(&mut self) -> SysLoad {
        // SAFETY: open query.
        let r = unsafe { PdhCollectQueryData(self.query) };
        if r != 0 || !std::mem::replace(&mut self.primed, true) {
            return SysLoad::default();
        }
        SysLoad {
            cpu_utility_pct: value(self.utility),
            clock_pct: value(self.performance),
        }
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        // SAFETY: we own the query (closing it also frees its counters).
        unsafe {
            let _ = PdhCloseQuery(self.query);
        }
    }
}
