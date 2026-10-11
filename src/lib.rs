use std::cmp::PartialEq;
use sha2::{Digest, Sha256};
use std::ffi::{c_char, c_int};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::slice;
use nusb::MaybeFuture;
use serde::Deserialize;
use std::sync::Mutex;
use std::thread::sleep;
use anyhow::anyhow;
use espflash::connection::{ResetAfterOperation, ResetBeforeOperation};

pub trait FpgaProgressCallbacks : Send {
    fn init(&mut self, message: &str, max_progress: u64);
    fn update(&mut self, progress: u64);
    fn message(&mut self, message: &str);
    fn error(&mut self, message: &str);

    fn success(&mut self);
}

static FPGA_PROGRESS: Mutex<Option<&'static mut dyn FpgaProgressCallbacks>> = Mutex::new(None);

type PAPIProgressCallback = Option<unsafe extern "C" fn(u64)>;
type PAPIProgressResetCallback = Option<unsafe extern "C" fn(*const c_char, u16, u64)>;
type PAPIStringCallback = Option<unsafe extern "C" fn(*const c_char, u16)>;

unsafe extern "C" {
    pub fn papi_fpga_program_sram(
        path: *const c_char,
        path_len: usize,
        message_callback: PAPIStringCallback,
        error_callback: PAPIStringCallback,
        progress_reset_callback: PAPIProgressResetCallback,
        progress_callback: PAPIProgressCallback,
    ) -> c_int;

    pub fn papi_fpga_program_flash(
        path: *const c_char,
        path_len: usize,
        message_callback: PAPIStringCallback,
        error_callback: PAPIStringCallback,
        progress_reset_callback: PAPIProgressResetCallback,
        progress_callback: PAPIProgressCallback,
    ) -> c_int;

    pub fn papi_fpga_reset() -> c_int;
}

unsafe extern "C" fn fpga_loader_message(msg: *const c_char, msg_len: u16) {
    let bytes = unsafe { slice::from_raw_parts(msg as *const u8, msg_len as usize) };
    let s = unsafe {  std::str::from_utf8_unchecked(bytes) };
    if let Ok(Some(callbacks)) = FPGA_PROGRESS.lock().as_deref_mut() {
        callbacks.message(s);
    }
}

unsafe extern "C" fn fpga_loader_error(msg: *const c_char, msg_len: u16) {
    let bytes = unsafe { slice::from_raw_parts(msg as *const u8, msg_len as usize) };
    let s = unsafe { std::str::from_utf8_unchecked(bytes) };
    if let Ok(Some(callbacks)) = FPGA_PROGRESS.lock().as_deref_mut() {
        callbacks.error(s);
    }
}

unsafe extern "C" fn fpga_loader_progress_reset(msg: *const c_char, msg_len: u16, max_progress: u64) {
    let bytes = unsafe { slice::from_raw_parts(msg as *const u8, msg_len as usize) };
    let s = unsafe { std::str::from_utf8_unchecked(bytes) };

    if let Ok(Some(callbacks)) = FPGA_PROGRESS.lock().as_deref_mut() {
        callbacks.init(s, max_progress);
    }
}

unsafe extern "C" fn fpga_loader_progress(progress: u64) {
    if let Ok(Some(callbacks)) = FPGA_PROGRESS.lock().as_deref_mut() {
        callbacks.update(progress);
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct FirmwareVersion {
    pub version: String,
    pub fpga_url: String,
    pub fpga_sha256: String,
    pub mcu_url: String,
    pub mcu_sha256: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Firmware {
    pub id: String,
    pub title: String,
    pub description: String,
    pub versions: Vec<FirmwareVersion>,
}

pub fn fpga_local_filename(fw: &Firmware, ver: &FirmwareVersion) -> String {
    format!("fpga-{}-{}.fs", fw.id, &ver.fpga_sha256[..8])
}

pub fn mcu_local_filename(fw: &Firmware, ver: &FirmwareVersion) -> String {
    format!("mcu-{}-{}.bin", fw.id, &ver.mcu_sha256[..8])
}

#[derive(Debug, Deserialize)]
struct Config {
    firmware: Vec<Firmware>,
}

const CONFIG_TOML: &str = include_str!("../config.toml");
pub fn get_firmware_options() -> Vec<Firmware> {
    let config: Config = toml::from_str(CONFIG_TOML).unwrap();
    let by_ref: std::collections::HashMap<String, FirmwareVersion> = config
        .firmware
        .iter()
        .flat_map(|fw| {
            fw.versions.iter().map(|ver| {
                (format!("ref://{}/{}", fw.id, ver.version), ver.clone())
            })
        })
        .collect();

    config.firmware
        .into_iter()
        .map(|mut fw| {
            for ver in &mut fw.versions {
                if ver.mcu_url.starts_with("ref://") {
                    ver.mcu_sha256 = by_ref[&ver.mcu_url].mcu_sha256.clone();
                    ver.mcu_url = by_ref[&ver.mcu_url].mcu_url.clone();
                }
                if ver.fpga_url.starts_with("ref://") {
                    ver.fpga_sha256 = by_ref[&ver.fpga_url].fpga_sha256.clone();
                    ver.fpga_url = by_ref[&ver.fpga_url].fpga_url.clone();
                }
            }
            fw
        })
        .collect()
}

#[derive(Clone, Debug, clap::ValueEnum, Eq, PartialEq)]
pub enum Mode {
    WriteFlash,
    WriteSRAM,
    DownloadOnly,
}

#[derive(Eq, PartialEq, Copy, Clone, Debug)]
pub enum Target {
    Flash,
    SRAM,
}

struct FpgaProgressResetGuard;
impl Drop for FpgaProgressResetGuard {
    fn drop(&mut self) {
        *FPGA_PROGRESS.lock().unwrap() = None;
    }
}

pub fn flash_fpga(
    fpga_path: &Path,
    target: Target,
    progress: Option<&mut dyn FpgaProgressCallbacks>
) -> anyhow::Result<()> {
    if let Some(p) = progress {
        unsafe {
            let ptr: *mut dyn FpgaProgressCallbacks = &mut *p;
            let static_ptr: &'static mut dyn FpgaProgressCallbacks = std::mem::transmute(ptr);
            *FPGA_PROGRESS.lock().unwrap() = Some(static_ptr);
        }
    }

    let _reset_guard = FpgaProgressResetGuard;

    let path_bytes = fpga_path.as_os_str().as_encoded_bytes();

    let status = unsafe {
        let flash_fn = match target {
            Target::Flash => papi_fpga_program_flash,
            Target::SRAM => papi_fpga_program_sram,
        };
        flash_fn(
            path_bytes.as_ptr() as *const c_char,
            path_bytes.len() as usize,
            Some(fpga_loader_message),
            Some(fpga_loader_error),
            Some(fpga_loader_progress_reset),
            Some(fpga_loader_progress),
        )
    };

    if status == 1 {
        if let Ok(Some(callbacks)) = FPGA_PROGRESS.lock().as_deref_mut() {
            callbacks.success();
        }
        if target == Target::Flash {
            if let Ok(Some(callbacks)) = FPGA_PROGRESS.lock().as_deref_mut() {
                callbacks.message("Rebooting FPGA...");
            }
            unsafe {
                papi_fpga_reset();
            }
            for _ in 0..30 {
                if count_chromatics()? == (DeviceCounts { chromatic: 1, gwu2x: 1 }) {
                    break;
                }
                sleep(std::time::Duration::from_millis(100));
            }
        }
        Ok(())
    } else {
        Err(anyhow!("openFPGALoader failed with status: {status}"))
    }
}

pub fn flash_mcu(
    mcu_path: &Path,
    progress: &mut dyn espflash::target::ProgressCallbacks,
) -> anyhow::Result<()> {
    let (port_name, usb_info) = serialport::available_ports()?
        .into_iter()
        .find_map(|port|
            if let serialport::SerialPortType::UsbPort(info) = &port.port_type {
                if is_chromatic(info.vid, info.pid) {
                    Some((port.port_name, info.clone()))
                } else {
                    None
                }
            } else {
                None
            }
        ).ok_or(anyhow!("USB serial device not found"))?;
    
    let serial = espflash::connection::Port::open(&serialport::new(port_name, 115_200))?;
    let connection = espflash::connection::Connection::new(serial, usb_info, ResetAfterOperation::HardReset, ResetBeforeOperation::DefaultReset, 115_200);

    let mut flasher = espflash::flasher::Flasher::connect(connection, true, true, false, None, None)?;

    let data = fs::read(mcu_path)?;
    flasher.write_bin_to_flash(0, data.as_slice(), progress)?;

    Ok(())
}

fn is_chromatic(vendor_id: u16, product_id: u16) -> bool {
    if vendor_id != 0x374E {
        return false;
    }
    // The player number is part of the product ID when re-plugged
    product_id >= 0x0101 && product_id <= 0x0108
}

fn is_gwu2x(vendor_id: u16, product_id: u16) -> bool {
    // GoWin GWU2X (programming interface for GoWin GW5A-25A)
    vendor_id == 0x33AA && product_id == 0x0120
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct DeviceCounts {
    pub chromatic: usize,
    pub gwu2x: usize,
}

pub fn count_chromatics() -> anyhow::Result<DeviceCounts> {
    let mut ret = DeviceCounts { chromatic: 0, gwu2x: 0 };

    for device in nusb::list_devices().wait()? {
        if is_chromatic(device.vendor_id(), device.product_id()) {
            ret.chromatic += 1;
        } else if is_gwu2x(device.vendor_id(), device.product_id()) {
            ret.gwu2x += 1;
        }
    }
    Ok(ret)
}

pub enum HashOutcome {
    Match,
    Mismatch { actual: String },
}

pub fn verify_hash(file_path: &Path, expected_hash: &str) -> anyhow::Result<HashOutcome> {
    let mut file = File::open(file_path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    let actual_hash = hex::encode(hasher.finalize());

    if actual_hash.eq_ignore_ascii_case(expected_hash) {
        Ok(HashOutcome::Match)
    } else {
        Ok(HashOutcome::Mismatch { actual: actual_hash })
    }
}
