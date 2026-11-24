//! The GPU temperature and power straight from the Nvidia driver (`nvml.dll`, in System32 with every driver).
//!
//! Much cheaper than LibreHardwareMonitor for the taskbar: LHM updates every value of the card (~90 ms), NVML
//! gives the one temperature in a fraction of a millisecond – the same value as "GPU Core". Needs no admin rights.
//! Without an Nvidia card there is no `Nvml` and everything goes through the bridge.

use std::ffi::c_void;

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

type Init = unsafe extern "C" fn() -> i32;
type Handle = unsafe extern "C" fn(u32, *mut *mut c_void) -> i32;
type Temperature = unsafe extern "C" fn(*mut c_void, u32, *mut u32) -> i32;
type Power = unsafe extern "C" fn(*mut c_void, *mut u32) -> i32;

pub struct Nvml {
    device: *mut c_void,
    temperature: Temperature,
    power: Power,
    shutdown: Init,
}

// The handle is only a number the driver gave out; NVML may be called from any thread.
unsafe impl Send for Nvml {}

impl Nvml {
    /// The first Nvidia card – `None` without one.
    pub fn open() -> Option<Nvml> {
        unsafe {
            let name: Vec<u16> = "nvml.dll\0".encode_utf16().collect();
            // Only from System32: a nvml.dll next to the exe is not taken.
            let lib = LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
            if lib.is_null() {
                return None;
            }
            let get = |name: &[u8]| GetProcAddress(lib, name.as_ptr());
            let init: Init = std::mem::transmute(get(b"nvmlInit_v2\0")?);
            let handle: Handle = std::mem::transmute(get(b"nvmlDeviceGetHandleByIndex_v2\0")?);
            let temperature: Temperature = std::mem::transmute(get(b"nvmlDeviceGetTemperature\0")?);
            let power: Power = std::mem::transmute(get(b"nvmlDeviceGetPowerUsage\0")?);
            let shutdown: Init = std::mem::transmute(get(b"nvmlShutdown\0")?);
            if init() != 0 {
                return None;
            }
            let mut device = std::ptr::null_mut();
            if handle(0, &mut device) != 0 {
                shutdown();
                return None;
            }
            Some(Nvml { device, temperature, power, shutdown })
        }
    }

    /// °C of the GPU core.
    pub fn temperature(&self) -> Option<f64> {
        let mut t = 0u32;
        // 0 = NVML_TEMPERATURE_GPU; 0 = NVML_SUCCESS
        (unsafe { (self.temperature)(self.device, 0, &mut t) } == 0).then_some(t as f64)
    }

    /// Watts of the whole card (NVML reports milliwatts).
    pub fn power(&self) -> Option<f64> {
        let mut mw = 0u32;
        (unsafe { (self.power)(self.device, &mut mw) } == 0).then_some(mw as f64 / 1000.0)
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        unsafe { (self.shutdown)() };
    }
}

#[cfg(test)]
mod tests {
    /// On a PC with an Nvidia card (the developer's) it reads; elsewhere it is simply not there.
    #[test]
    fn reads_if_there() {
        if let Some(n) = super::Nvml::open() {
            let t = n.temperature().expect("a temperature");
            assert!((10.0..110.0).contains(&t), "{t}");
            assert!(n.power().is_some_and(|w| (0.0..1000.0).contains(&w)));
        }
    }
}
