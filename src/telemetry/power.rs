//! Wattage, read from AMD's Display Library.
//!
//! Windows has no API for this. `GetSystemPowerStatus` answers a battery, not a
//! budget, and the `Power Meter` performance-counter set exists but has no
//! instances on a desktop — that counter is where a PSU's draw would be read
//! from if a desktop had one, and it does not.
//!
//! On an AMD machine the one user-mode source is `atiadlxx.dll`, the driver's
//! own library, which exposes the same PMLog telemetry the Radeon overlay
//! draws. It is loaded at runtime and never linked: the DLL is absent on any
//! machine without a Radeon driver, and a monitor that refuses to start is
//! worse than one with a missing card.
//!
//! Only AMD is covered. NVML would be the equivalent for a GeForce card and is
//! a second DLL behind the same shape if one ever turns up.

use std::ffi::c_void;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::{s, w};

use super::PowerSample;

/// `ADL_PMLOG_ASIC_POWER` — the package's own draw, in whole watts. This is the
/// one sensor that means the same thing on both kinds of adapter here.
const ASIC_POWER: usize = 23;
/// `ADL_PMLOG_FAN_RPM`. Its *presence* is the tell: only a card with a fan on
/// it reports a fan speed.
const FAN_RPM: usize = 14;
/// The driver's sensor table is a fixed 256 entries.
const MAX_SENSORS: usize = 256;

/// ADL's per-sensor record: whether the driver can answer, and the answer.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SensorValue {
    supported: i32,
    value: i32,
}

/// `ADLPMLogDataOutput`. `size` must hold the *documented* length of the
/// structure, not of this allocation — the driver reads that field, and the
/// sensor table it is told about ends exactly where this one does.
#[repr(C)]
struct PMLogData {
    size: i32,
    sensors: [SensorValue; MAX_SENSORS],
}

impl PMLogData {
    fn ask() -> Self {
        Self {
            size: (4 + MAX_SENSORS * 8) as i32,
            sensors: [SensorValue::default(); MAX_SENSORS],
        }
    }

    fn watts(&self, sensor: usize) -> Option<u32> {
        let s = self.sensors.get(sensor)?;
        if s.supported == 0 {
            return None;
        }
        // No consumer part draws four kilowatts, so a number that large is the
        // driver answering with something that is not a wattage.
        match s.value {
            0..=4096 => Some(s.value as u32),
            _ => None,
        }
    }
}

// ADL allocates its own state through this and releases it through the same
// handle, so it has to be the C runtime's allocator — Rust's is a different
// one, and a block allocated in one and freed in the other is heap corruption
// rather than a leak.
unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
}

unsafe extern "C" fn adl_malloc(size: i32) -> *mut c_void {
    // SAFETY: `malloc` is the C runtime's, already linked into this process.
    unsafe { malloc(size.max(0) as usize) }
}

/// Which adapter is which, decided once.
///
/// `ADL_PMLOG_ASIC_POWER` is the whole package on an APU — the processor's
/// graphics half shares the die and the power rails, which is why that reading
/// climbs under a CPU-only load — and the card's own board power on a discrete
/// one. The fan sensor is what separates them.
///
/// ponytail: the tell is the fan, so a passively cooled card, or one whose fan
/// the driver does not expose, is read as a processor instead. Reading the PCI
/// bus and device numbers out of `ADLAdapterInfo` would tell the two apart
/// properly; it is a struct layout to get right, and it buys nothing on a
/// machine that has one of each.
fn role(has_asic_power: bool, has_fan: bool) -> Role {
    match (has_asic_power, has_fan) {
        (true, false) => Role::Cpu,
        (true, true) => Role::Gpu,
        _ => Role::Other,
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Role {
    Cpu,
    Gpu,
    Other,
}

struct Adl {
    /// Held, never released: the pointers below live inside this module, and an
    /// unloaded DLL would leave them dangling. One handle per process is the
    /// whole cost, and the process is the app.
    _module: HMODULE,
    query: unsafe extern "C" fn(*mut c_void, i32, *mut PMLogData) -> i32,
    cpu: Option<i32>,
    gpu: Option<i32>,
}

impl Adl {
    fn load() -> Option<Self> {
        // SAFETY: the module name is a live, nul-terminated wide literal, and
        // every symbol below is checked for before it is called.
        unsafe {
            let module = LoadLibraryW(w!("atiadlxx.dll")).ok()?;

            let create: unsafe extern "C" fn(unsafe extern "C" fn(i32) -> *mut c_void, i32) -> i32 =
                std::mem::transmute(GetProcAddress(module, s!("ADL_Main_Control_Create"))?);
            let count: unsafe extern "C" fn(*mut i32) -> i32 = std::mem::transmute(
                GetProcAddress(module, s!("ADL_Adapter_NumberOfAdapters_Get"))?,
            );
            // Optional: an older driver that lacks PMLog has no wattage to give
            // and this whole collector is absent, which is a correct answer.
            let query: unsafe extern "C" fn(*mut c_void, i32, *mut PMLogData) -> i32 =
                std::mem::transmute(GetProcAddress(module, s!("ADL2_New_QueryPMLogData_Get"))?);

            // The null context is deliberate — PMLog is answered per adapter
            // index, and this is the one call that does not need a session.
            if create(adl_malloc, 0) != 0 {
                return None;
            }

            let mut n = 0i32;
            if count(&mut n) != 0 || n <= 0 {
                return None;
            }

            let mut cpu = None;
            let mut gpu = None;
            for adapter in 0..n {
                let mut data = PMLogData::ask();
                if query(std::ptr::null_mut(), adapter, &mut data) != 0 {
                    continue;
                }
                let has = |i: usize| data.sensors[i].supported != 0;
                match role(has(ASIC_POWER), has(FAN_RPM)) {
                    // The several adapters of one physical card all answer
                    // identically — and report the same number — so the first
                    // is the card.
                    Role::Cpu if cpu.is_none() => cpu = Some(adapter),
                    Role::Gpu if gpu.is_none() => gpu = Some(adapter),
                    _ => {}
                }
            }
            if cpu.is_none() && gpu.is_none() {
                return None;
            }
            Some(Self {
                _module: module,
                query,
                cpu,
                gpu,
            })
        }
    }

    fn watts(&self, adapter: i32) -> Option<u32> {
        let mut data = PMLogData::ask();
        // SAFETY: `query` came from this module and takes a struct of this
        // exact type; `data` outlives the call.
        let rc = unsafe { (self.query)(std::ptr::null_mut(), adapter, &mut data) };
        if rc != 0 {
            return None;
        }
        data.watts(ASIC_POWER)
    }
}

/// Processor and graphics wattage, as the driver reports it.
pub struct Power {
    adl: Option<Adl>,
}

impl Default for Power {
    fn default() -> Self {
        Self::new()
    }
}

impl Power {
    /// Loads the driver's library, or gives up quietly.
    ///
    /// The lookup and the adapter classification both happen here rather than
    /// on every poll: which adapter is which cannot change while the process
    /// runs, and re-deciding it once a second would be eight wasted calls for
    /// an answer that is already known.
    pub fn new() -> Self {
        Self { adl: Adl::load() }
    }

    pub fn poll(&mut self) -> Option<PowerSample> {
        let adl = self.adl.as_ref()?;
        let cpu_w = adl.cpu.and_then(|i| adl.watts(i));
        let gpu_w = adl.gpu.and_then(|i| adl.watts(i));
        if cpu_w.is_none() && gpu_w.is_none() {
            return None;
        }
        Some(PowerSample { cpu_w, gpu_w })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The classification is the one part of this that is a guess about the
    /// hardware rather than a number copied out of a header, so it is the part
    /// that is pinned.
    #[test]
    fn the_fan_sensor_is_what_separates_a_card_from_a_processor() {
        assert_eq!(role(true, false), Role::Cpu);
        assert_eq!(role(true, true), Role::Gpu);
        assert_eq!(role(false, true), Role::Other);
        assert_eq!(role(false, false), Role::Other);
    }

    /// A sensor the driver does not support is not a zero-watt reading — the
    /// value beside it is whatever was in the struct.
    #[test]
    fn an_unsupported_sensor_reads_as_absent_rather_than_zero() {
        let mut data = PMLogData::ask();
        assert_eq!(data.watts(ASIC_POWER), None);
        data.sensors[ASIC_POWER] = SensorValue {
            supported: 1,
            value: 0,
        };
        assert_eq!(data.watts(ASIC_POWER), Some(0));
    }

    /// A garbage value must not reach the page as a wattage.
    #[test]
    fn an_impossible_wattage_is_dropped() {
        let mut data = PMLogData::ask();
        data.sensors[ASIC_POWER] = SensorValue {
            supported: 1,
            value: -1,
        };
        assert_eq!(data.watts(ASIC_POWER), None);
        data.sensors[ASIC_POWER] = SensorValue {
            supported: 1,
            value: 60_000,
        };
        assert_eq!(data.watts(ASIC_POWER), None);
    }

    /// The size field is what the driver reads; a struct that grew past it
    /// would be a buffer it no longer agrees with.
    #[test]
    fn the_size_field_matches_the_documented_struct() {
        assert_eq!(PMLogData::ask().size, 4 + 256 * 8);
    }
}
