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
use clap::{Parser, arg, ValueEnum};

static C_PROGRESS_BAR: Mutex<Option<ProgressBar>> = Mutex::new(None);
pub type PAPIProgressCallback = Option<unsafe extern "C" fn(u64)>;
pub type PAPIProgressResetCallback = Option<unsafe extern "C" fn(*const c_char, u16, u64)>;
pub type PAPIStringCallback = Option<unsafe extern "C" fn(*const c_char, u16)>;

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

const COLOR_RESET: &str = "\x1b[0m";
const COLOR_RED: &str = "\x1b[31;1m";
const COLOR_GREEN: &str = "\x1b[32;1m";
const COLOR_YELLOW: &str = "\x1b[33;1m";
const COLOR_CYAN: &str = "\x1b[36;1m";
const COLOR_BOLD: &str = "\x1b[1m";
const COLOR_DIM: &str = "\x1b[2m";

fn linkify(url: impl std::fmt::Display, label: impl std::fmt::Display) -> String {
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, label)
}

unsafe extern "C" fn on_loader_message(msg: *const c_char, msg_len: u16) {
    let bytes = unsafe { slice::from_raw_parts(msg as *const u8, msg_len as usize) };
    let s = unsafe {  std::str::from_utf8_unchecked(bytes) };
    println!("{s}");
}
unsafe extern "C" fn on_loader_error(msg: *const c_char, msg_len: u16) {
    let bytes = unsafe { slice::from_raw_parts(msg as *const u8, msg_len as usize) };
    let s = unsafe { std::str::from_utf8_unchecked(bytes) };
    println!("{COLOR_RED}{s}{COLOR_RESET}");
}

unsafe extern "C" fn on_loader_progress_reset(msg: *const c_char, msg_len: u16, max_progress: u64) {
    let bytes = unsafe { slice::from_raw_parts(msg as *const u8, msg_len as usize) };
    let s = unsafe { std::str::from_utf8_unchecked(bytes) };

    if let Ok(mut guard) = C_PROGRESS_BAR.lock() {
        let style = ProgressStyle::default_bar()
            .template("{msg:.cyan} [{elapsed_precise}] [{wide_bar:.cyan/blue}] ({eta} remaining)")
            .unwrap()
            .progress_chars("##-");
        let pb = ProgressBar::new(max_progress)
            .with_style(style)
            .with_message(s.to_string());
        *guard = Some(pb);
    }
}

unsafe extern "C" fn on_loader_progress(progress: u64) {
    if let Ok(guard) = C_PROGRESS_BAR.lock() {
        if let Some(pb) = guard.as_ref() {
            pb.set_position(progress as u64);
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Firmware {
    pub id: String,
    pub title: String,
    pub version: String,
    pub description: String,
    pub fpga_url: String,
    pub fpga_sha256: String,
}

impl Firmware {
    fn fpga_local_filename(&self) -> String {
        format!("fpga-{}-{}.fs", self.id, &self.fpga_sha256[..8])
    }
}

#[derive(Debug, Deserialize)]
struct Config {
    firmware: Vec<Firmware>,
}

const CONFIG_TOML: &str = include_str!("../config.toml");
fn get_firmware_options() -> Vec<Firmware> {
    let config: Config = toml::from_str(CONFIG_TOML).unwrap();
    config.firmware
}


fn wait_for_exit() {
    println!("\nPress enter to exit.");
    std::io::stdin().read_line(&mut String::new()).unwrap();
}

#[derive(Clone, Debug, ValueEnum, Eq, PartialEq)]
enum Mode {
    WriteFlash,
    WriteSRAM,
    DownloadOnly,
}

#[derive(Eq, PartialEq)]
enum Target {
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

    #[arg(long, help = "Select the specified firmware number")]
    firmware: Option<usize>,
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    enable_ansi_support();
    let fw_options = get_firmware_options();
    let args = Args::parse();

    let before_exit : fn() = if args.no_pause {
        || {}
    } else {
        wait_for_exit
    };

    if args.target().is_some() && !have_single_chromatic_target()? {
        before_exit();
        return Ok(());
    }

    println!("{COLOR_YELLOW}=========================================={COLOR_RESET}");
    println!("{COLOR_YELLOW} Select Firmware Option:{COLOR_RESET}");
    println!("{COLOR_YELLOW}=========================================={COLOR_RESET}\n");

    for (i, fw) in fw_options.iter().enumerate() {
        println!("{COLOR_GREEN}{}{COLOR_RESET}) {COLOR_BOLD}{} {}{COLOR_RESET}", i + 1, fw.title, fw.version);
        println!("   {COLOR_DIM}{}{COLOR_RESET}\n", fw.description);
    }

    let selected_fw = loop {
        print!("Enter choice ({COLOR_GREEN}1{COLOR_RESET}-{COLOR_GREEN}{}{COLOR_RESET}): ", fw_options.len());
        io::stdout().flush()?;

        if let Some(choice) = args.firmware {
            if choice >= 1 && choice <= fw_options.len() {
                break &fw_options[choice - 1];
            }
            println!("Invalid firmware specified on command line");
            before_exit();
            return Err("Invalid firmware on command line".into());
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

    let fw_path = download_dir.join(selected_fw.fpga_local_filename());
    let fw_ok = ensure_available(&selected_fw.fpga_url, &selected_fw.fpga_sha256, &fw_path, &selected_fw.title)?;

    println!();
    if !fw_ok {
        eprintln!(
            "\n{COLOR_RED}Verification failed; you might want to delete {}.{COLOR_RESET}",
            linkify(download_dir.display(), fw_path.display())
        );
        before_exit();
        return Err("Bad Hash".into());
    }

    if args.mode == Mode::DownloadOnly {
        println!("Downloaded to {}", linkify(download_dir.display(), fw_path.display()));
        before_exit();
        return Ok(());
    }

    println!("\n{COLOR_YELLOW}=========================================={COLOR_RESET}");
    println!("{COLOR_YELLOW} Ready to Program Firmware {COLOR_RESET}");
    println!("{COLOR_YELLOW}=========================================={COLOR_RESET}");
    println!("Name:    {}", selected_fw.title);
    println!("Version: {}", selected_fw.version);
    println!("File:    {}", linkify(download_dir.display(), fw_path.display()));

    let target = args.target().unwrap();
    match target {
        Target::Flash => {
            println!("\n{COLOR_RED}Writing to FLASH{COLOR_RESET} - if you decide to undo this change, you will need to re-flash the previous firmware.");
        }
        Target::SRAM => {
            println!("\n{COLOR_GREEN}Writing to SRAM{COLOR_RESET} - this firmware change will be undone when you turn off your console.");
        }
    }


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

    // 5. Execute openFPGALoader passing the firmware path
    println!("\n{COLOR_GREEN}Programming FPGA...{COLOR_RESET}\n");

    let fw_path_bytes = fw_path.as_os_str().as_encoded_bytes();

    unsafe {
        let flash_fn = match target {
            Target::Flash => papi_fpga_program_flash,
            Target::SRAM => papi_fpga_program_sram,
        };
        let status = flash_fn(
            fw_path_bytes.as_ptr() as *const c_char,
            fw_path_bytes.len() as usize,
            Some(on_loader_message),
            Some(on_loader_error),
            Some(on_loader_progress_reset),
            Some(on_loader_progress),
        );

        if status == 1 {
            println!("\n{COLOR_GREEN}Update complete.{COLOR_RESET}");
            if target == Target::Flash {
                println!("Rebooting FPGA...");
                papi_fpga_reset();
            }
        } else {
            println!("\n{COLOR_RED}openFPGALoader failed with status: {status}{COLOR_RESET}");
        }
    }

    before_exit();
    Ok(())
}

fn have_single_chromatic_target() -> Result<bool, Box<dyn std::error::Error>> {
    let devices : Vec<_> = nusb::list_devices().wait()?.collect();

    let chromatics = devices.iter().filter(|device| {
       device.vendor_id() == 0x374E && device.product_id() == 0x0101
    }).count();
    if chromatics == 0 {
        println!("{COLOR_RED}No Chromatics were found via USB. Is your console plugged in?{COLOR_RESET}");
        return Ok(false);
    }
    if chromatics != 1 {
        println!("{COLOR_RED}Found {} Chromatic devices, required exactly 1.{COLOR_RESET}", chromatics);
        return Ok(false);
    }


    let fpga_interfaces = devices.iter().filter(|device| {
        // GoWin GWU2X (programming interface for GoWin GW5A-25A)
        device.vendor_id() == 0x33AA && device.product_id() == 0x0120
    }).count();
    if fpga_interfaces != 1 {
        println!("{COLOR_RED}Found {} GoWin FPGA interfaces, required exactly 1.{COLOR_RESET}", fpga_interfaces);
        return Ok(false);
    }
    println!("{COLOR_GREEN}Found 1 Chromatic device and 1 GoWin FPGA interface.{COLOR_RESET}");
    Ok(true)
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
        if verify_hash(path, hash)? {
            return Ok(true);
        }

        println!("\n{COLOR_RED}Incorrect hash, removing {}...{COLOR_RESET}", path.display());
        fs::remove_file(path)?;
    }

    println!("\n{COLOR_CYAN}Fetching {title}...{COLOR_RESET}");
    println!("    {COLOR_DIM}{url}{COLOR_RESET}");
    fetch_resource(url, path)?;
    Ok(verify_hash(path, hash)?)
}


fn verify_hash(file_path: &Path, expected_hash: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let file_name = file_path.file_name().unwrap().to_string_lossy();
    println!("{COLOR_CYAN}Verifying hash for {file_name}...{COLOR_RESET}");

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
        println!("{COLOR_GREEN}Verification OK.{COLOR_RESET}");
        Ok(true)
    } else {
        println!("{COLOR_RED}Verification FAILED!{COLOR_RESET}");
        println!("  Expected: {expected_hash}");
        println!("  Actual:   {actual_hash}");
        Ok(false)
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
