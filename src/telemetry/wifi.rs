//! Wi-Fi collector: SSID, signal quality and frequency band of the interface
//! that is currently associated.
//!
//! Everything here goes through the WLAN API, which needs a client handle.
//! The handle is opened once in `new()` and closed in `Drop` — reopening per
//! poll would allocate a driver context a second time, every second.
//!
//! On a machine with no wireless adapter (a desktop on Ethernet) every call
//! fails or reports zero interfaces, and `poll()` simply returns `None`.

use super::{Band, WifiSample};
use std::mem::size_of;
use std::ptr;
use windows::core::GUID;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::WiFi::{
    wlan_intf_opcode_channel_number, wlan_intf_opcode_current_connection, wlan_interface_state_connected,
    WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle, WlanQueryInterface,
    WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST,
};

/// Value documented by Microsoft for a client that understands the current API.
const WLAN_CLIENT_VERSION: u32 = 2;

/// Map a channel number to its band.
///
/// Boundaries follow the 802.11 channel plan: 1-14 is 2.4 GHz, 5 GHz starts at
/// 32 (36 is the first usable bonding channel) and runs to 177, and anything
/// above that is 6 GHz. Channel 0 means "unknown" — the driver's way of saying
/// it has no number to report.
fn band_for_channel(channel: u32) -> Band {
    match channel {
        1..=14 => Band::B2G4,
        32..=177 => Band::B5G,
        178..=u32::MAX => Band::B6G,
        _ => Band::Unknown,
    }
}

/// Decode a `DOT11_SSID` payload: the first `len` bytes of the 32-byte buffer,
/// lossily as UTF-8 (an SSID is an arbitrary byte string, not necessarily text).
fn decode_ssid(buf: &[u8; 32], len: u32) -> Option<String> {
    let len = (len as usize).min(buf.len());
    if len == 0 {
        return None;
    }
    let s = String::from_utf8_lossy(&buf[..len]).into_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// SSID, band and signal for the connected WLAN interface.
pub struct Wifi {
    /// `None` when the WLAN service is unavailable (no card, or the service is
    /// stopped) — kept so we do not retry opening it on every poll.
    handle: Option<HANDLE>,
}

impl Default for Wifi {
    fn default() -> Self {
        // SAFETY: out-params are live locals; `preserved` is documented to be
        // null and the API ignores it.
        let handle = unsafe {
            let mut handle = HANDLE::default();
            let mut negotiated = 0u32;
            let rc = WlanOpenHandle(
                WLAN_CLIENT_VERSION,
                None,
                &mut negotiated,
                &mut handle,
            );
            (rc == 0 && !handle.is_invalid()).then_some(handle)
        };
        Self { handle }
    }
}

impl Drop for Wifi {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: the handle came from WlanOpenHandle and is closed once.
            unsafe {
                let _ = WlanCloseHandle(handle, None);
            }
        }
    }
}

impl Wifi {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current connection, or `None` when not associated / no WLAN hardware.
    ///
    /// Each query runs in its own block *after* the previous buffer has been
    /// released, so no early return can leave an allocation behind.
    pub fn poll(&mut self) -> Option<WifiSample> {
        let handle = self.handle?;
        let guid = connected_interface(handle)?;

        let (ssid, signal_pct) = query_connection(handle, &guid)?;
        Some(WifiSample {
            ssid,
            band: query_band(handle, &guid),
            signal_pct,
        })
    }
}

/// GUID of the first interface in the connected state, if any.
fn connected_interface(handle: HANDLE) -> Option<GUID> {
    // SAFETY: `list` is written by the API and released here before returning;
    // the slice is bounded by dwNumberOfItems, which matches the allocation.
    unsafe {
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = ptr::null_mut();
        if WlanEnumInterfaces(handle, None, &mut list) != 0 || list.is_null() {
            return None;
        }
        let interfaces =
            std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), (*list).dwNumberOfItems as usize);
        let guid = interfaces
            .iter()
            .find(|i| i.isState == wlan_interface_state_connected)
            .map(|i| i.InterfaceGuid);
        WlanFreeMemory(list.cast());
        guid
    }
}

/// SSID and signal quality of the current association.
fn query_connection(handle: HANDLE, guid: &GUID) -> Option<(Option<String>, Option<u32>)> {
    // SAFETY: the handle is live; `data` is released before we return.
    unsafe {
        let mut size = 0u32;
        let mut data: *mut core::ffi::c_void = ptr::null_mut();
        let rc = WlanQueryInterface(
            handle,
            guid,
            wlan_intf_opcode_current_connection,
            None,
            &mut size,
            &mut data,
            None,
        );
        let ok = rc == 0
            && !data.is_null()
            && (size as usize) >= size_of::<WLAN_CONNECTION_ATTRIBUTES>();
        if !ok {
            if !data.is_null() {
                WlanFreeMemory(data);
            }
            return None;
        }
        let assoc = &(*(data as *const WLAN_CONNECTION_ATTRIBUTES)).wlanAssociationAttributes;
        let ssid = decode_ssid(&assoc.dot11Ssid.ucSSID, assoc.dot11Ssid.uSSIDLength);
        let signal_pct = (assoc.wlanSignalQuality <= 100).then_some(assoc.wlanSignalQuality);
        WlanFreeMemory(data);
        Some((ssid, signal_pct))
    }
}

/// Band of the channel the interface is camped on.
///
/// The opcode returns a bare `u32` in the buffer, not a struct.
fn query_band(handle: HANDLE, guid: &GUID) -> Option<Band> {
    // SAFETY: the handle is live; `data` is released before we return.
    unsafe {
        let mut size = 0u32;
        let mut data: *mut core::ffi::c_void = ptr::null_mut();
        let rc = WlanQueryInterface(
            handle,
            guid,
            wlan_intf_opcode_channel_number,
            None,
            &mut size,
            &mut data,
            None,
        );
        let ok = rc == 0 && !data.is_null() && (size as usize) >= size_of::<u32>();
        if !ok {
            if !data.is_null() {
                WlanFreeMemory(data);
            }
            return None;
        }
        let channel = *(data as *const u32);
        WlanFreeMemory(data);
        match band_for_channel(channel) {
            Band::Unknown => None,
            band => Some(band),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_point_four_gigahertz_channels() {
        assert_eq!(band_for_channel(1), Band::B2G4);
        assert_eq!(band_for_channel(6), Band::B2G4);
        assert_eq!(band_for_channel(14), Band::B2G4);
    }

    #[test]
    fn five_gigahertz_channels() {
        assert_eq!(band_for_channel(36), Band::B5G);
        assert_eq!(band_for_channel(100), Band::B5G);
        assert_eq!(band_for_channel(177), Band::B5G);
    }

    #[test]
    fn six_gigahertz_channels() {
        assert_eq!(band_for_channel(181), Band::B6G);
        assert_eq!(band_for_channel(233), Band::B6G);
    }

    #[test]
    fn boundaries_are_exact() {
        // Just outside 2.4 GHz, and the gap between 14 and 32 is unknown.
        assert_eq!(band_for_channel(15), Band::Unknown);
        assert_eq!(band_for_channel(31), Band::Unknown);
        assert_eq!(band_for_channel(32), Band::B5G);
        // 6 GHz begins one channel past the top of 5 GHz.
        assert_eq!(band_for_channel(178), Band::B6G);
    }

    #[test]
    fn zero_channel_is_unknown() {
        assert_eq!(band_for_channel(0), Band::Unknown);
    }

    #[test]
    fn ssid_is_trimmed_to_its_length() {
        let mut buf = [0u8; 32];
        buf[..5].copy_from_slice(b"home!");
        assert_eq!(decode_ssid(&buf, 5).as_deref(), Some("home!"));
    }

    #[test]
    fn zero_length_ssid_is_none() {
        assert_eq!(decode_ssid(&[0u8; 32], 0), None);
    }

    #[test]
    fn oversized_length_cannot_read_past_the_buffer() {
        assert_eq!(decode_ssid(&[b'a'; 32], 999).as_deref(), Some(&"a".repeat(32)[..]));
    }

    #[test]
    fn non_utf8_ssid_is_replaced_not_dropped() {
        let mut buf = [0u8; 32];
        buf[..3].copy_from_slice(&[0xff, 0xfe, b'x']);
        assert!(decode_ssid(&buf, 3).is_some());
    }
}
