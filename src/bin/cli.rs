use std::{fs, io};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use anyhow::{anyhow, bail};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use url::Url;
use chromatic_ez_firmware as lib;
use lib::{count_chromatics, Target, HashOutcome, FpgaProgressCallbacks, get_firmware_options, Mode};

pub mod colors {
    pub const COLOR_RESET: &str = "\x1b[0m";
    pub const COLOR_RED: &str = "\x1b[31;1m";
    pub const COLOR_GREEN: &str = "\x1b[32;1m";
    pub const COLOR_YELLOW: &str = "\x1b[33;1m";
    pub const COLOR_CYAN: &str = "\x1b[36;1m";
    pub const COLOR_BOLD: &str = "\x1b[1m";
    pub const COLOR_DIM: &str = "\x1b[2m";
}

pub fn linkify(url: impl std::fmt::Display, label: impl std::fmt::Display) -> String {
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, label)
}

pub fn have_single_chromatic_target() -> anyhow::Result<bool> {
    use colors::*;

    let counts = count_chromatics()?;

    if counts.chromatic == 0 {
       println!("{COLOR_RED}No Chromatics were found via USB. Is your console plugged in?{COLOR_RESET}");
        return Ok(false);
    }
    if counts.chromatic != 1 {
        println!("{COLOR_RED}Found {} Chromatic devices, required exactly 1.{COLOR_RESET}", counts.chromatic);
        return Ok(false);
    }


    if counts.gwu2x != 1 {
        println!("{COLOR_RED}Found {} GoWin FPGA interfaces, required exactly 1.{COLOR_RESET}", counts.gwu2x);
        return Ok(false);
    }
    println!("{COLOR_GREEN}Found 1 Chromatic device and 1 GoWin FPGA interface.{COLOR_RESET}");
    Ok(true)
}

pub struct McuProgress {
    pb: Option<ProgressBar>,
}

impl Default for McuProgress {
    fn default() -> Self {
        Self { pb: None }
    }
}

pub struct FpgaProgress {
    progress_bar: Option<ProgressBar>,
}

impl Default for FpgaProgress {
    fn default() -> Self {
        Self { progress_bar: None }
    }
}

impl FpgaProgressCallbacks for FpgaProgress {
    fn init(&mut self, message: &str, max_progress: u64) {
        let style = ProgressStyle::default_bar()
            .template("{msg:.cyan} [{elapsed_precise}] [{wide_bar:.cyan/blue}] ({eta} remaining)")
            .unwrap()
            .progress_chars("##-");
        let pb = ProgressBar::new(max_progress)
            .with_style(style)
            .with_message(message.to_string());
        self.progress_bar = Some(pb);
    }

    fn update(&mut self, progress: u64) {
        if let Some(pb) = &mut self.progress_bar {
            pb.set_position(progress);
        }
    }

    fn message(&mut self, message: &str) {
        println!("{message}");
    }

    fn error(&mut self, message: &str) {
        use colors::*;
        println!("{COLOR_RED}{message}{COLOR_RESET}");
    }

    fn success(&mut self) {
        use colors::*;
        println!("\n{COLOR_GREEN}FPGA update complete.{COLOR_RESET}");
    }
}

impl espflash::target::ProgressCallbacks for McuProgress {
    fn init(&mut self, addr: u32, len: usize) {
        let style = ProgressStyle::default_bar()
            .template("{msg:.cyan} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta} remaining)")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("##-");
        let pb = ProgressBar::new(len as u64)
            .with_style(style)
            .with_message(format!("Flashing MCU at {addr:#X}"));
        self.pb = Some(pb);
    }

    fn update(&mut self, current: usize) {
        if let Some(ref pb) = self.pb {
            pb.set_position(current as u64);
        }
    }

    fn verifying(&mut self) {
        if let Some(ref pb) = self.pb {
            pb.set_message("Verifying...");
        }
    }

    fn finish(&mut self, _skipped: bool) {
        if let Some(ref pb) = self.pb {
            pb.finish_and_clear();
        }
    }
}

pub fn flash_fpga(fpga_path: &Path, target: Target) -> anyhow::Result<()> {
    use colors::*;
    println!("\n{COLOR_GREEN}Programming FPGA...{COLOR_RESET}\n");

    let ret = chromatic_ez_firmware::flash_fpga(fpga_path, target, Some(&mut FpgaProgress::default()));
    match ret {
        Ok(()) => Ok(()),
        Err(e) => {
            println!("\n{COLOR_RED}openFPGALoader failed with status: {e}{COLOR_RESET}");
            Err(e)
        }
    }
}

pub fn flash_mcu(mcu_path: &Path) -> anyhow::Result<()> {
    use colors::*;

    println!("\n{COLOR_GREEN}Programming MCU...{COLOR_RESET}\n");
    match lib::flash_mcu(mcu_path, &mut McuProgress::default()) {
        Ok(()) => {
            println!("\n{COLOR_GREEN}MCU update complete.{COLOR_RESET}");
            Ok(())
        },
        Err(e) => {
            println!("\n{COLOR_RED}MCU update failed: {e}{COLOR_RESET}");
            Err(e)
        }
    }
}

pub fn verify_hash(file_path: &Path, expected: &str) -> anyhow::Result<bool> {
    use colors::*;

    let file_name = file_path.file_name().unwrap().to_string_lossy();
    println!("{COLOR_CYAN}Verifying hash for {file_name}...{COLOR_RESET}");

    match lib::verify_hash(file_path, expected)? {
        HashOutcome::Match => {
            println!("{COLOR_GREEN}Verification OK.{COLOR_RESET}");
            Ok(true)
        },
        HashOutcome::Mismatch { actual} => {
            println!("{COLOR_RED}Verification FAILED!{COLOR_RESET}");
            println!("  Expected: {expected}");
            println!("  Actual:   {actual}");
            Ok(false)
        }
    }
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    enable_ansi_support();

    use colors::*;

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

    let download_dir = lib::download_dir()?;

    let fpga_path = download_dir.join(lib::fpga_local_filename(selected_fw, selected_version));
    let fpga_ok = ensure_available(&selected_version.fpga_url, &selected_version.fpga_sha256, &fpga_path, &format!("{} - FPGA", &selected_fw.title))?;
    let mcu_path = download_dir.join(lib::mcu_local_filename(selected_fw, selected_version));
    let mcu_ok= ensure_available(&selected_version.mcu_url, &selected_version.mcu_sha256, &mcu_path, &format!("{} - MCU", &selected_fw.title))?;

    println!();
    if !(fpga_ok && mcu_ok) {
        eprintln!(
            "\n{COLOR_RED}Verification failed; you might want to delete {}.{COLOR_RESET}",
            linkify(download_dir.display(), fpga_path.display())
        );
        before_exit();
        return Err("Bad Hash".into());
    }

    if args.mode == Mode::DownloadOnly {
        println!("Downloaded FPGA to {}", linkify(download_dir.display(), fpga_path.display()));
        println!("Downloaded MCU to {}", linkify(download_dir.display(), mcu_path.display()));
        before_exit();
        return Ok(());
    }

    println!("\n{COLOR_YELLOW}=========================================={COLOR_RESET}");
    println!("{COLOR_YELLOW} Ready to Program Firmware {COLOR_RESET}");
    println!("{COLOR_YELLOW}=========================================={COLOR_RESET}");
    println!("Name:    {}", selected_fw.title);
    println!("Version: {}", selected_version.version);
    println!("FPGA:    {}", linkify(download_dir.display(), fpga_path.display()));
    println!("MCU:     {}", linkify(download_dir.display(), mcu_path.display()));

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

    flash_fpga(&fpga_path, target)?;
    if target == Target::Flash {
        flash_mcu(&mcu_path)?;
    } else {
        println!("\n{COLOR_RED}Skipping MCU, can't write to SRAM{COLOR_RESET}");
    }

    println!("\n{COLOR_YELLOW}Turn the power switch off, take out the batteries, wait a few minutes, then turn the power switch back on.{COLOR_RESET}");
    before_exit();
    Ok(())
}

fn fetch_resource(url_str: &str, dest: &PathBuf) -> anyhow::Result<()> {
    let url = Url::parse(url_str)?;

    if url.scheme() == "file" {
        let local_path = url
            .to_file_path()
            .map_err(|_| anyhow!("Failed to convert file:// URL to local path"))?;
        if !local_path.exists() {
            bail!("Local file not found: {}", local_path.display());
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

fn ensure_available(url: &str, hash: &str, path: &PathBuf, title: &str) -> anyhow::Result<bool> {
    use colors::*;

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

fn wait_for_exit() {
    println!("\nPress enter to exit.");
    std::io::stdin().read_line(&mut String::new()).unwrap();
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
