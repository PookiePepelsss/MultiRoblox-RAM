// Live load figures for the Accounts toolbar (whole machine) and the running
// cards (per client). Every figure is the change since the previous call, so
// the first call after start returns nothing for it.
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// Percent of the machine busy between two (idle, total) time samples.
fn busy_percent(prev: (u64, u64), now: (u64, u64)) -> Option<f64> {
    let idle = now.0.checked_sub(prev.0)?;
    let total = now.1.checked_sub(prev.1)?;
    (total > 0).then(|| (100.0 * total.saturating_sub(idle) as f64 / total as f64).clamp(0.0, 100.0))
}

/// A process's share of the whole machine, the way Task Manager shows it.
fn process_percent(cpu_100ns: u64, elapsed_secs: f64, cores: usize) -> Option<f64> {
    (elapsed_secs > 0.0 && cores > 0)
        .then(|| (cpu_100ns as f64 / 1e7 / (elapsed_secs * cores as f64) * 100.0).clamp(0.0, 100.0))
}

/// GPU counters arrive per process per engine. Processes on one engine add up,
/// and the busiest engine is the GPU's figure, as in Task Manager.
fn gpu_percent<'a>(items: impl Iterator<Item = (&'a str, f64)>) -> f64 {
    let mut engines: HashMap<&str, f64> = HashMap::new();
    for (name, value) in items {
        let engine = name.find("luid_").map_or(name, |at| &name[at..]);
        *engines.entry(engine).or_default() += value;
    }
    engines.values().copied().fold(0.0, f64::max).clamp(0.0, 100.0)
}

static CPU_SAMPLES: Lazy<Mutex<HashMap<u32, (u64, Instant)>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static SYSTEM_SAMPLE: Mutex<Option<(u64, u64)>> = Mutex::new(None);

/// CPU percent per pid, for the pids sampled on an earlier call as well.
pub(super) fn process_cpu(pids: &[u32]) -> HashMap<u32, f64> {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut samples = CPU_SAMPLES.lock().unwrap_or_else(|e| e.into_inner());
    samples.retain(|pid, _| pids.contains(pid));
    let mut out = HashMap::new();
    for &pid in pids {
        let Some(cpu) = sys::cpu_time_of(pid) else {
            continue;
        };
        let now = Instant::now();
        if let Some((prev, at)) = samples.insert(pid, (cpu, now)) {
            let percent = process_percent(cpu.saturating_sub(prev), now.duration_since(at).as_secs_f64(), cores);
            if let Some(percent) = percent {
                out.insert(pid, percent);
            }
        }
    }
    out
}

fn cpu_load() -> Option<f64> {
    let now = sys::system_times()?;
    let mut last = SYSTEM_SAMPLE.lock().unwrap_or_else(|e| e.into_inner());
    last.replace(now).and_then(|prev| busy_percent(prev, now))
}

/// { cpu, memory, gpu } in percent; a figure Windows can't give is null.
pub fn system_stats() -> Value {
    json!({ "cpu": cpu_load(), "memory": sys::memory_load(), "gpu": sys::gpu_load() })
}

#[cfg(windows)]
mod sys {
    use once_cell::sync::Lazy;
    use std::sync::Mutex;
    use windows::core::w;
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Performance::{
        PdhAddEnglishCounterW, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
        PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
    };
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows::Win32::System::Threading::{
        GetProcessTimes, GetSystemTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const PDH_MORE_DATA: u32 = 0x8000_07D2;

    fn ticks(t: FILETIME) -> u64 {
        (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
    }

    pub fn cpu_time_of(pid: u32) -> Option<u64> {
        let (mut created, mut exited, mut kernel, mut user) = Default::default();
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let ok = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user).is_ok();
            let _ = CloseHandle(handle);
            ok.then(|| ticks(kernel) + ticks(user))
        }
    }

    /// (idle, total) time. Kernel time already includes idle time.
    pub fn system_times() -> Option<(u64, u64)> {
        let (mut idle, mut kernel, mut user) = Default::default();
        unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
        Some((ticks(idle), ticks(kernel) + ticks(user)))
    }

    pub fn memory_load() -> Option<f64> {
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
        (status.ullTotalPhys > 0).then(|| {
            100.0 * status.ullTotalPhys.saturating_sub(status.ullAvailPhys) as f64 / status.ullTotalPhys as f64
        })
    }

    struct GpuQuery {
        query: isize,
        counter: isize,
    }

    // Opened once and kept: these are rate counters, measured between collects.
    static GPU: Lazy<Mutex<Option<GpuQuery>>> = Lazy::new(|| Mutex::new(open_gpu_query()));

    fn open_gpu_query() -> Option<GpuQuery> {
        let (mut query, mut counter) = (0isize, 0isize);
        unsafe {
            if PdhOpenQueryW(None, 0, &mut query) != 0 {
                return None;
            }
            // The English path, since counter names are translated on other Windows languages.
            let path = w!("\\GPU Engine(*engtype_3D)\\Utilization Percentage");
            (PdhAddEnglishCounterW(query, path, 0, &mut counter) == 0).then_some(GpuQuery { query, counter })
        }
    }

    pub fn gpu_load() -> Option<f64> {
        let guard = GPU.lock().unwrap_or_else(|e| e.into_inner());
        let gpu = guard.as_ref()?;
        unsafe {
            if PdhCollectQueryData(gpu.query) != 0 {
                return None;
            }
            let (mut size, mut count) = (0u32, 0u32);
            let status = PdhGetFormattedCounterArrayW(gpu.counter, PDH_FMT_DOUBLE, &mut size, &mut count, None);
            if status != PDH_MORE_DATA || size == 0 {
                return None;
            }
            // Holds pointers and doubles, so it needs 8-byte alignment.
            let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
            let items = buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            if PdhGetFormattedCounterArrayW(gpu.counter, PDH_FMT_DOUBLE, &mut size, &mut count, Some(items)) != 0 {
                return None;
            }
            let read: Vec<(String, f64)> = std::slice::from_raw_parts(items, count as usize)
                .iter()
                .filter(|item| item.FmtValue.CStatus <= 1) // valid or new data
                .filter_map(|item| Some((item.szName.to_string().ok()?, item.FmtValue.Anonymous.doubleValue)))
                .collect();
            Some(super::gpu_percent(read.iter().map(|(name, value)| (name.as_str(), *value))))
        }
    }
}

#[cfg(not(windows))]
mod sys {
    pub fn cpu_time_of(_pid: u32) -> Option<u64> {
        None
    }
    pub fn system_times() -> Option<(u64, u64)> {
        None
    }
    pub fn memory_load() -> Option<f64> {
        None
    }
    pub fn gpu_load() -> Option<f64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_load_is_the_busy_share_of_elapsed_time() {
        assert_eq!(busy_percent((100, 1000), (150, 1200)), Some(75.0));
        assert_eq!(busy_percent((100, 1000), (300, 1200)), Some(0.0));
        assert_eq!(busy_percent((100, 1000), (100, 1000)), None, "no time passed");
        assert_eq!(busy_percent((100, 1000), (50, 900)), None, "counters went backwards");
    }

    #[test]
    fn process_load_is_a_share_of_every_core() {
        // One full second of CPU over one second on four cores is a quarter of the machine.
        assert_eq!(process_percent(10_000_000, 1.0, 4), Some(25.0));
        assert_eq!(process_percent(90_000_000, 1.0, 4), Some(100.0), "capped");
        assert_eq!(process_percent(10_000_000, 0.0, 4), None);
    }

    #[test]
    fn gpu_takes_the_busiest_engine() {
        let items = [
            ("pid_1_luid_0x0_0xA_phys_0_eng_0_engtype_3D", 10.0),
            ("pid_2_luid_0x0_0xA_phys_0_eng_0_engtype_3D", 20.5),
            ("pid_2_luid_0x0_0xB_phys_0_eng_0_engtype_3D", 12.0),
        ];
        assert_eq!(gpu_percent(items.into_iter()), 30.5);
        assert_eq!(gpu_percent(std::iter::empty()), 0.0);
    }
}
