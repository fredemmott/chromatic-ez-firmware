mod cli;

use std::cmp::PartialEq;
use sha2::{Digest, Sha256};
use std::ffi::{c_char, c_int};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::slice;
use nusb::MaybeFuture;
use url::Url;
use serde::Deserialize;
use indicatif::{ProgressBar, ProgressStyle};
use std::sync::Mutex;
use std::thread::sleep;
use anyhow::anyhow;
use clap::{Parser, ValueEnum};
use espflash::connection::{ResetAfterOperation, ResetBeforeOperation};

use crate::cli::colors::*;

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

fn fpga_local_filename(fw: &Firmware, ver: &FirmwareVersion) -> String {
    format!("fpga-{}-{}.fs", fw.id, &ver.fpga_sha256[..8])
}

fn mcu_local_filename(fw: &Firmware, ver: &FirmwareVersion) -> String {
    format!("mcu-{}-{}.bin", fw.id, &ver.mcu_sha256[..8])
}

#[derive(Debug, Deserialize)]
struct Config {
    firmware: Vec<Firmware>,
}

const CONFIG_TOML: &str = include_str!("../config.toml");
fn get_firmware_options() -> Vec<Firmware> {
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

fn wait_for_exit() {
    println!("\nPress enter to exit.");
    std::io::stdin().read_line(&mut String::new()).unwrap();
}

#[derive(Clone, Debug, ValueEnum, Eq, PartialEq)]
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

#[derive(Parser, Debug)]
struct Args {
    #[arg(short, long, value_enum, default_value_t = Mode::WriteFlash)]
    mode: Mode,

    #[arg(short, long, help = "Skip confirmation prompts")]
    yes: bool,

    #[arg(long, help = "Skip 'Press enter to exit' prompts")]
    no_pause: bool,

    #[arg(long, help = "Select the specified firmware by ID")]
    firmware: Option<String>,

    #[arg(long, help = "Select the specified firmware version ")]
    firmware_version: Option<String>,
}
impl Args {
    fn target(&self) -> Option<Target> {
        match self.mode {
            Mode::WriteFlash => Some(Target::Flash),
            Mode::WriteSRAM => Some(Target::SRAM),
            Mode::DownloadOnly => None,
        }
    }
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

pub (crate) fn flash_mcu(mcu_path: &Path) -> anyhow::Result<()> {
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
    let mut progress = cli::McuProgress::default();

    let data = fs::read(mcu_path)?;
    flasher.write_bin_to_flash(0, data.as_slice(), &mut progress)?;

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    enable_ansi_support();
    let fw_options = get_firmware_options();
    let args = Args::parse();

    let before_exit : fn() = if args.no_pause {
        || {}
    } else {
        wait_for_exit
    };

    if args.target().is_some() && !cli::have_single_chromatic_target()? {
        before_exit();
        return Ok(());
    }

    println!("{COLOR_YELLOW}=========================================={COLOR_RESET}");
    println!("{COLOR_YELLOW} Select Firmware:{COLOR_RESET}");
    println!("{COLOR_YELLOW}=========================================={COLOR_RESET}\n");

    for (i, fw) in fw_options.iter().enumerate() {
        println!("{COLOR_GREEN}{}{COLOR_RESET}) {COLOR_BOLD}{}{COLOR_RESET}", i + 1, fw.title);
        println!("   {COLOR_DIM}{}{COLOR_RESET}\n", fw.description);
    }

    let selected_fw = loop {
        print!("Enter choice ({COLOR_GREEN}1{COLOR_RESET}-{COLOR_GREEN}{}{COLOR_RESET}): ", fw_options.len());
        io::stdout().flush()?;

        if let Some(choice) = &args.firmware {
            match fw_options.iter().find(|f| { &f.id == choice }) {
                Some(fw) => break fw,
                None => {
                    println!("Invalid firmware specified on command line");
                    before_exit();
                    return Err("Invalid firmware on command line".into());
                }
            }
        } else {
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;

            if let Ok(choice) = input.trim().parse::<usize>() {
                if choice >= 1 && choice <= fw_options.len() {
                    break &fw_options[choice - 1];
                }
            }
            println!("Invalid selection, please try again.");
        }
    };

    let selected_version = if selected_fw.versions.len() == 1 {
        &selected_fw.versions[0]
    } else {
        loop {
            println!("\n{COLOR_YELLOW}=========================================={COLOR_RESET}");
            println!("{COLOR_YELLOW} Select Version:{COLOR_RESET}");
            println!("{COLOR_YELLOW}=========================================={COLOR_RESET}\n");
            for (i, ver) in selected_fw.versions.iter().enumerate() {
                print!("{COLOR_GREEN}{}{COLOR_RESET}) {COLOR_BOLD}{} {}{COLOR_RESET}", i + 1, selected_fw.title, ver.version);
                if i == 0 {
                    println!(" {COLOR_DIM}(default){COLOR_RESET}");
                } else {
                    println!();
                }
            }
            print!("Enter choice ({COLOR_GREEN}1{COLOR_RESET}-{COLOR_GREEN}{}{COLOR_RESET}) or hit enter for default: ", selected_fw.versions.len());
            io::stdout().flush()?;
            if let Some(choice) = &args.firmware_version {
                match selected_fw.versions.iter().find(|v| { &v.version == choice }) {
                    Some(ver) => break ver,
                    None => {
                        println!("Invalid firmware version specified on command line");
                        before_exit();
                        return Err("Invalid firmware version on command line".into());
                    }
                }
            } else {
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                let input = input.trim();

                if let Ok(choice) = input.parse::<usize>() {
                    if choice >= 1 && choice <= selected_fw.versions.len() {
                        break &selected_fw.versions[choice - 1];
                    }
                } else if input.is_empty() {
                    break &selected_fw.versions[0];
                }
                println!("Invalid selection, please try again.");
            }
        }
    };

    let download_dir = {
        let this_exe = std::env::current_exe()?;
        let mut folder_name = this_exe.file_stem().unwrap().to_os_string();
        folder_name.push("-downloads");
        let mut path = std::env::current_dir()?.join(&folder_name);

        if fs::create_dir_all(&path).is_err() {
            path = std::env::temp_dir().join(&folder_name)
        }
        fs::create_dir_all(&path)?;
        path
    };

    let fpga_path = download_dir.join(fpga_local_filename(selected_fw, selected_version));
    let fpga_ok = ensure_available(&selected_version.fpga_url, &selected_version.fpga_sha256, &fpga_path, &format!("{} - FPGA", &selected_fw.title))?;
    let mcu_path = download_dir.join(mcu_local_filename(selected_fw, selected_version));
    let mcu_ok= ensure_available(&selected_version.mcu_url, &selected_version.mcu_sha256, &mcu_path, &format!("{} - MCU", &selected_fw.title))?;

    println!();
    if !(fpga_ok && mcu_ok) {
        eprintln!(
            "\n{COLOR_RED}Verification failed; you might want to delete {}.{COLOR_RESET}",
            cli::linkify(download_dir.display(), fpga_path.display())
        );
        before_exit();
        return Err("Bad Hash".into());
    }

    if args.mode == Mode::DownloadOnly {
        println!("Downloaded FPGA to {}", cli::linkify(download_dir.display(), fpga_path.display()));
        println!("Downloaded MCU to {}", cli::linkify(download_dir.display(), mcu_path.display()));
        before_exit();
        return Ok(());
    }

    println!("\n{COLOR_YELLOW}=========================================={COLOR_RESET}");
    println!("{COLOR_YELLOW} Ready to Program Firmware {COLOR_RESET}");
    println!("{COLOR_YELLOW}=========================================={COLOR_RESET}");
    println!("Name:    {}", selected_fw.title);
    println!("Version: {}", selected_version.version);
    println!("FPGA:    {}", cli::linkify(download_dir.display(), fpga_path.display()));
    println!("MCU:     {}", cli::linkify(download_dir.display(), mcu_path.display()));

    let target = args.target().unwrap();

    print!("\nDo you want to proceed? ({COLOR_RED}y{COLOR_RESET}/{COLOR_GREEN}N{COLOR_RESET}): ");
    io::stdout().flush()?;

    if args.yes {
        println!("{COLOR_YELLOW}'--yes' passed on command line{COLOR_RESET}")
    } else {
        let mut confirm = String::new();
        io::stdin().read_line(&mut confirm)?;
        let confirm = confirm.trim().to_lowercase();

        if confirm != "y" && confirm != "yes" {
            return Ok(());
        }
    }

    cli::flash_fpga(&fpga_path, target)?;
    if target == Target::Flash {
        cli::flash_mcu(&mcu_path)?;
    } else {
        println!("\n{COLOR_RED}Skipping MCU, can't write to SRAM{COLOR_RESET}");
    }

    println!("\n{COLOR_YELLOW}Turn the power switch off, take out the batteries, wait a few minutes, then turn the power switch back on.{COLOR_RESET}");
    before_exit();
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
struct DeviceCounts {
    chromatic: usize,
    gwu2x: usize,
}

fn count_chromatics() -> anyhow::Result<DeviceCounts> {
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

fn fetch_resource(url_str: &str, dest: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let url = Url::parse(url_str)?;

    if url.scheme() == "file" {
        let local_path = url
            .to_file_path()
            .map_err(|_| "Failed to convert file:// URL to local path")?;
        if !local_path.exists() {
            return Err(format!("Local file not found: {}", local_path.display()).into());
        }
        fs::copy(&local_path, dest)?;
    } else {
        let mut response = reqwest::blocking::get(url_str)?;

        let size = response.content_length().unwrap_or(0);
        let pb = ProgressBar::new(size);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("[{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")?
                .progress_chars("##-")
        );

        let mut reader = pb.wrap_read(&mut response);
        let mut file = File::create(dest)?;
        std::io::copy(&mut reader, &mut file)?;
    }
    Ok(())
}

fn ensure_available(url: &str, hash: &str, path: &PathBuf, title: &str) -> Result<bool, Box<dyn std::error::Error>> {
    if path.exists() {
        if cli::verify_hash(path, hash)? {
            return Ok(true);
        }

        println!("\n{COLOR_RED}Incorrect hash, removing {}...{COLOR_RESET}", path.display());
        fs::remove_file(path)?;
    }

    println!("\n{COLOR_CYAN}Fetching {title}...{COLOR_RESET}");
    println!("    {COLOR_DIM}{url}{COLOR_RESET}");
    fetch_resource(url, path)?;
    Ok(cli::verify_hash(path, hash)?)
}

pub (crate) enum HashOutcome {
    Match,
    Mismatch { actual: String },
}

pub (crate) fn verify_hash(file_path: &Path, expected_hash: &str) -> anyhow::Result<HashOutcome> {
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

/// Set `VIRTUAL_TERMINAL_PROCESSING` for compatibility with classic Windows cmd.exe; unneeded
/// but harmless on modern Windows Terminal
#[cfg(target_os = "windows")]
fn enable_ansi_support() {
    use std::os::windows::io::AsRawHandle;
    type HANDLE = *mut std::ffi::c_void;

    unsafe extern "system" {
        fn GetConsoleMode(handle: HANDLE, mode_pointer: *mut u32) -> i32;
        fn SetConsoleMode(handle: HANDLE, mode: u32) -> i32;
    }

    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    unsafe {
        let handle = std::io::stdout().as_raw_handle() as HANDLE;
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) != 0 {
            SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn enable_ansi_support() {}
